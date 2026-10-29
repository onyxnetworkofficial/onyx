// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxLedger, IDisputesBinding} from "../src/OnyxLedger.sol";
import {OnyxSettlementTest} from "./OnyxSettlement.t.sol";
import {OnyxSettlement, IERC20Min} from "../src/OnyxSettlement.sol";
import {OnyxRound} from "../src/OnyxRound.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";
import {OnyxSig} from "../src/OnyxSig.sol";

/// Regression tests from the M6 loop-35 review (M6-L35-F1..F6). The `Harness` exposes the
/// incremental `_assess` next to the loop-34 O(members x reveals) logic as a reference, so a
/// differential fuzz pins the tallies to the original rule.
/// Exposes the new `_assess` and the loop-34 O(members x reveals) logic as a reference.
contract Harness is OnyxSettlement {
    constructor(
        IERC20Min t,
        address c,
        address x,
        string memory d,
        uint64 u,
        uint128 a,
        uint128 b,
        uint128 s
    ) OnyxSettlement(t, c, x, d, u, a, b, s) {}

    function assessNew(uint64 id, bytes32 shard)
        external
        view
        returns (uint8[] memory, uint256, uint256)
    {
        Assessment memory a = _assess(id, shard, _rounds[id], _shards[id][shard]);
        return (a.verdict, a.agrees, a.pool);
    }

    /// Reference panel pool (Rust L29-F4, M6-L2-F1), counted in full: members who neither
    /// revealed as a miner nor committed a verdict on the shard, whose stake less exposure
    /// and this shard's missed slash covers `panelSlash`.
    function poolRef(uint64 id, bytes32 shard) external view returns (uint256 n) {
        Round storage r = _rounds[id];
        Shard storage s = _shards[id][shard];
        address[] memory members = r.members;
        for (uint256 i = 0; i < members.length; i++) {
            address v = members[i];
            bool skip;
            for (uint256 j = 0; j < s.reveals.length; j++) {
                address m = s.reveals[j].miner;
                if (m == v || verdicts[id][shard][m][v].commit != bytes32(0)) skip = true;
            }
            if (skip) continue;
            uint256 taken = IDisputesBinding(disputes).exposure(v);
            if (_oldOne(id, shard, v) == 3) taken += r.cfg.validation.missedSlash;
            if (validators[v].stake >= taken + r.cfg.dispute.panelSlash) n++;
        }
    }

    function assessOld(uint64 id, bytes32 shard)
        external
        view
        returns (uint8[] memory verdict, uint256 agrees)
    {
        address[] memory members = _rounds[id].members;
        verdict = new uint8[](members.length);
        for (uint256 i = 0; i < members.length; i++) {
            verdict[i] = _oldOne(id, shard, members[i]);
            if (verdict[i] == 1) agrees++;
        }
    }

    function _oldOne(uint64 id, bytes32 shard, address v) internal view returns (uint8) {
        Round storage r = _rounds[id];
        Shard storage s = _shards[id][shard];
        bool assignedAny;
        bool answered;
        bool unrevealed;
        for (uint256 j = 0; j < s.reveals.length; j++) {
            address m = s.reveals[j].miner;
            if (!OnyxRound.assigned(r.seed, v, shard, m, r.cfg.validation.ratePpm)) continue;
            assignedAny = true;
            VerdictRec storage rec = verdicts[id][shard][m][v];
            if (rec.commit == bytes32(0)) continue;
            if (!rec.revealed) {
                unrevealed = true;
                continue;
            }
            if (rec.kind == OnyxRound.AGREE) answered = true;
        }
        // Rust (M6-F7): a correct Agree earns its fee; an unrevealed commit counts as answered
        // (no missed slash) and is slashed at round level instead.
        if (answered) return 1;
        if (unrevealed) return 0;
        if (assignedAny) return 3;
        return 0;
    }

    function sortGas(address[] memory a) external view returns (uint256 g, address[] memory) {
        g = gasleft();
        _heapSort(a);
        g -= gasleft();
        return (g, a);
    }
}

contract OnyxSettlementL35Test is OnyxSettlementTest {
    uint256 internal constant GAS_CAP = 15_000_000;

    Harness internal h;

    function _etch() internal {
        // The harness's own extension carries the same parameters, so the fallback at `st`
        // still runs the extension code on `st`'s storage.
        Harness tmp = new Harness(
            IERC20Min(address(usdc)),
            address(this),
            address(dsp),
            "onyx.devnet",
            50,
            1_000,
            1_000,
            500
        );
        vm.etch(address(st), address(tmp).code);
        h = Harness(address(st));
    }

    // ---------------------------------------------------------------- differential _assess

    function _sid(uint256 k) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("vs", k));
    }

    address[] internal fms;
    bool[] internal frv;
    uint64 internal frate;
    uint256 internal fseed;

    function _fOpen(uint8 extraVals, uint32 rate) internal {
        uint256 ev = bound(extraVals, 0, 6);
        for (uint256 i = 0; i < ev; i++) {
            address a = vm.addr(0xC000 + i);
            _fund(a, STAKE);
            vm.prank(a);
            st.stake(STAKE);
        }
        OnyxRound.RoundConfig memory c = _cfg();
        c.validation.minerReward = 1;
        frate = uint64(bound(rate, 1_000, 1_000_000));
        c.validation.ratePpm = frate;
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        st.openRound(ID, c, ids, inits);
    }

    function _fMiners(uint8 nMiners) internal {
        uint256 nm = bound(nMiners, 1, 6);
        for (uint256 j = 0; j < nm; j++) {
            uint256 k = 0xF000 + j;
            fms.push(vm.addr(k));
            _fund(fms[j], BOND);
            _commit(k, fms[j], _sid(j), fields.root);
        }
        vm.roll(base + 100);
        for (uint256 j = 0; j < nm; j++) {
            frv.push(uint256(keccak256(abi.encodePacked(fseed, "r", j))) % 4 != 0);
            if (frv[j]) _reveal(fms[j], _sid(j), fields.root);
        }
        vm.roll(base + 202);
        st.drawSeed(ID);
    }

    /// 0 none, 1 commit Agree only, 2..5 commit+reveal kind 0..3
    function _act(uint256 i, uint256 j) internal view returns (uint8) {
        address[] memory mem = st.roundMembers(ID);
        if (!frv[j] || !OnyxRound.assigned(st.seedOf(ID), mem[i], shard, fms[j], frate)) {
            return 0;
        }
        return uint8(uint256(keccak256(abi.encodePacked(fseed, i, j))) % 6);
    }

    function _kindRoot(uint8 a) internal pure returns (uint8 kind, bytes32 root) {
        kind = a == 1 ? 0 : a - 2;
        root = kind == OnyxRound.DISAGREE ? keccak256("other") : bytes32(0);
    }

    function _fCommit(uint256 i, uint256 j) internal {
        uint8 a = _act(i, j);
        if (a == 0) return;
        address v = st.roundMembers(ID)[i];
        (uint8 kind, bytes32 root) = _kindRoot(a);
        bytes32 hv = OnyxRound.verdictCommitHash(
            st.seedOf(ID), v, shard, fms[j], kind, root, _vsalt(v, fms[j])
        );
        vm.prank(v);
        st.commitVerdict(ID, shard, fms[j], hv);
    }

    function _fReveal(uint256 i, uint256 j) internal {
        uint8 a = _act(i, j);
        if (a < 2) return;
        address v = st.roundMembers(ID)[i];
        (uint8 kind, bytes32 root) = _kindRoot(a);
        vm.prank(v);
        st.revealVerdict(ID, shard, fms[j], kind, root, _vsalt(v, fms[j]));
    }

    /// Classes 0, 1, 3 are verdict classes; bit 2 marks a run with an unrevealed verdict commit
    /// (round-level slash, M6-F7) and bit 4 one where a validator both agreed and left a commit
    /// unrevealed on the shard (fee and round-level slash both apply).
    function test_AssessDiffCoverage() public {
        uint256[5] memory cnt;
        // 40 runs: each deploys a harness, and 60 no longer fit the per-test gas limit.
        for (uint256 k = 0; k < 40; k++) {
            uint256 snap = vm.snapshotState();
            uint256 r = uint256(keccak256(abi.encodePacked("cov", k)));
            uint256 cls = _diff(r, uint8(r >> 8), uint8(r >> 16), uint32(r >> 24));
            vm.revertToState(snap);
            for (uint256 b = 0; b < 5; b++) {
                if (cls & (1 << b) != 0) cnt[b]++;
            }
        }
        emit log_named_uint("runs with class0 none", cnt[0]);
        emit log_named_uint("runs with class1 agree", cnt[1]);
        emit log_named_uint("runs with an unrevealed commit", cnt[2]);
        emit log_named_uint("runs with class3 missed", cnt[3]);
        emit log_named_uint("runs with agree + unrevealed", cnt[4]);
        for (uint256 b = 0; b < 5; b++) {
            assertGt(cnt[b], 0, "every verdict class is exercised");
        }
    }

    function testFuzz_AssessMatchesReference(
        uint256 seed,
        uint8 extraVals,
        uint8 nMiners,
        uint32 rate
    ) public {
        _diff(seed, extraVals, nMiners, rate);
    }

    function _diff(uint256 seed, uint8 extraVals, uint8 nMiners, uint32 rate)
        internal
        returns (uint256 classes)
    {
        _etch();
        fseed = seed;
        _fOpen(extraVals, rate);
        _fMiners(nMiners);
        uint256 nv = st.roundMembers(ID).length;
        for (uint256 i = 0; i < nv; i++) {
            for (uint256 j = 0; j < fms.length; j++) {
                _fCommit(i, j);
            }
        }
        vm.roll(base + 300);
        for (uint256 i = 0; i < nv; i++) {
            for (uint256 j = 0; j < fms.length; j++) {
                _fReveal(i, j);
            }
        }
        vm.roll(base + 400);
        (uint8[] memory vn, uint256 an, uint256 pn) = h.assessNew(ID, shard);
        {
            uint256 cap = uint256(sx().roundConfig(ID).dispute.panelSize) + 1;
            uint256 pr = h.poolRef(ID, shard);
            assertEq(pn, pr < cap ? pr : cap, "panel pool, counted up to panelSize + 1");
        }
        (uint8[] memory vo, uint256 ao) = h.assessOld(ID, shard);
        assertEq(an, ao, "agrees");
        assertEq(vn.length, vo.length);
        address[] memory mem = st.roundMembers(ID);
        for (uint256 i = 0; i < vn.length; i++) {
            assertEq(vn[i], vo[i], "verdict class");
            classes |= 1 << vn[i];
            // The round-level counter matches the commits left unrevealed (M6-F7).
            uint256 unrevealed;
            for (uint256 j = 0; j < fms.length; j++) {
                if (_act(i, j) == 1) unrevealed++;
            }
            assertEq(st.unrevealedVerdicts(ID, mem[i]), unrevealed, "unrevealed counter");
            if (unrevealed > 0) classes |= 1 << 2;
            if (unrevealed > 0 && vn[i] == 1) classes |= 1 << 4;
        }
    }

    // ---------------------------------------------------------------- sort worst case (F4)

    /// Ranks that made every middle pivot of the old Hoare quicksort the minimum of its range
    /// (its 4x adversarial case). The heapsort must not care.
    function _killer(uint256 n) internal pure returns (uint256[] memory rank) {
        uint256[] memory ids = new uint256[](n);
        rank = new uint256[](n);
        for (uint256 i = 0; i < n; i++) {
            ids[i] = i;
        }
        uint256 hi = n - 1;
        for (uint256 lo = 0; lo < hi; lo++) {
            uint256 m = lo + (hi - lo) / 2;
            rank[ids[m]] = lo;
            (ids[lo], ids[m]) = (ids[m], ids[lo]);
        }
        rank[ids[hi]] = hi;
    }

    function _addrOfRank(uint256 r) internal pure returns (address) {
        return address(uint160(0x1000000000000000000000000000000000000000) + uint160(r + 1) * 7919);
    }

    function test_L35_F4_HeapSortKillerOrderGas() public {
        _etch();
        uint256 n = 128;
        uint256[] memory rank = _killer(n);
        address[] memory k = new address[](n);
        address[] memory srt = new address[](n);
        address[] memory rnd = new address[](n);
        for (uint256 i = 0; i < n; i++) {
            k[i] = _addrOfRank(rank[i]);
            srt[i] = _addrOfRank(i);
            rnd[i] = vm.addr(0xD000 + i);
        }
        (uint256 gK, address[] memory outK) = h.sortGas(k);
        (uint256 gS,) = h.sortGas(srt);
        (uint256 gR,) = h.sortGas(rnd);
        for (uint256 i = 1; i < n; i++) {
            assertLt(uint160(outK[i - 1]), uint160(outK[i]));
        }
        emit log_named_uint("sort gas killer", gK);
        emit log_named_uint("sort gas sorted", gS);
        emit log_named_uint("sort gas random", gR);
        // O(n log n) on every order: the old killer order cost 4.4x a random one.
        assertLt(gK, gR * 3 / 2, "killer order no longer special");
        assertLt(gS, gR * 3 / 2, "sorted order no longer special");
    }

    /// openRound at the bound with the registry staked in the quicksort-killer order.
    function test_L35_F4_OpenRoundGasAtBound_KillerOrder() public {
        for (uint256 i = 0; i < 3; i++) {
            vm.prank(vals[i]);
            sx().requestUnbond();
        }
        for (uint256 i = 0; i < standbyList.length; i++) {
            vm.prank(standbyList[i]);
            sx().requestUnbond();
        }
        uint256 n = 128;
        uint256[] memory rank = _killer(n);
        for (uint256 i = 0; i < n; i++) {
            address a = _addrOfRank(rank[i]);
            _fund(a, STAKE);
            vm.prank(a);
            st.stake(STAKE);
        }
        uint256 ns = st.MAX_SHARDS();
        bytes32[] memory ids = new bytes32[](ns);
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](ns);
        bytes32 fh = OnyxCommit.shardFieldsHash(fields);
        for (uint256 q = 0; q < ns; q++) {
            bytes32 ph = keccak256(abi.encodePacked("params", q));
            ids[q] = OnyxCommit.shardId(fh, ph, keccak256("registry"));
            inits[q] = OnyxLedger.ShardInit(fh, ph, keccak256("registry"));
        }
        for (uint256 i = 1; i < ns; i++) {
            for (uint256 j = i; j > 0 && ids[j - 1] > ids[j]; j--) {
                (ids[j - 1], ids[j]) = (ids[j], ids[j - 1]);
                (inits[j - 1], inits[j]) = (inits[j], inits[j - 1]);
            }
        }
        _fund(address(this), 900_000);
        st.fundRewards(900_000);
        OnyxRound.RoundConfig memory c = _cfg();
        uint256 g = gasleft();
        st.openRound(ID, c, ids, inits);
        uint256 used = g - gasleft();
        emit log_named_uint("openRound gas 128 x 64, killer order", used);
        assertLt(used, GAS_CAP, "openRound within budget at the bound in any staking order");
    }

    // ---------------------------------------------------------------- finalize worst case

    function _fill() internal {
        for (uint256 i = 3 + standbyList.length; i < 128; i++) {
            address a = vm.addr(0xD000 + i);
            _fund(a, STAKE);
            vm.prank(a);
            st.stake(STAKE);
        }
    }

    function _bound32(uint64 rate) internal returns (address[] memory ms) {
        OnyxRound.RoundConfig memory c = _cfg();
        c.validation.ratePpm = rate;
        c.validation.minerReward = 1;
        c.validation.missedSlash = 4_500; // pushes every silent member below the minimum
        c.validation.validatorSlash = 4_500;
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        st.openRound(ID, c, ids, inits);
        bytes32 tag = st.roundTag(ID);
        ms = new address[](32);
        for (uint256 j = 0; j < 32; j++) {
            ms[j] = vm.addr(0xE000 + j);
            _fund(ms[j], BOND);
            bytes32 hh = _commitHash(ms[j], keccak256(abi.encodePacked("s", j)), fields.root);
            (uint8 v, bytes32 r, bytes32 sg) = vm.sign(
                0xE000 + j, OnyxSig.eip191Digest(OnyxRound.commitMessage(tag, shard, hh, BOND))
            );
            st.commit(ID, shard, ms[j], hh, BOND, abi.encodePacked(r, sg, v));
        }
        vm.roll(base + 100);
        for (uint256 j = 0; j < 32; j++) {
            _reveal(ms[j], keccak256(abi.encodePacked("s", j)), fields.root);
        }
        vm.roll(base + 202);
        st.drawSeed(ID);
        _oneAgree(ms, rate);
        vm.roll(base + 450); // canaries unopened: settle also applies the missed deadline
    }

    function _oneAgree(address[] memory ms, uint64 rate) internal {
        address[] memory mem = st.roundMembers(ID);
        bytes32 seed = st.seedOf(ID);
        (uint256 i0, uint256 j0) = (0, 0);
        while (!OnyxRound.assigned(seed, mem[i0], shard, ms[j0], rate)) {
            j0++;
            if (j0 == ms.length) (i0, j0) = (i0 + 1, 0);
        }
        bytes32 hv = OnyxRound.verdictCommitHash(
            seed, mem[i0], shard, ms[j0], 0, bytes32(0), _vsalt(mem[i0], ms[j0])
        );
        vm.prank(mem[i0]);
        st.commitVerdict(ID, shard, ms[j0], hv);
        vm.roll(base + 300);
        vm.prank(mem[i0]);
        st.revealVerdict(ID, shard, ms[j0], 0, bytes32(0), _vsalt(mem[i0], ms[j0]));
    }

    /// Worst case for finalize (M6-L35-F3): 127 silent members each slashed below the minimum
    /// stake, so each one is also deactivated. Must stay within the per-transaction budget.
    /// Side effect: the registry collapses below `MIN_ROUND_MEMBERS` and no round can open
    /// until validators stake again (`TooFewMembers`).
    function test_L35_F3_FinalizeWorstCase_SlashCascade_Rate100() public {
        _fill();
        _bound32(1e6);
        uint256 g = gasleft();
        st.finalizeShard(ID, shard);
        uint256 used = g - gasleft();
        emit log_named_uint("finalize 128x32 rate 100%, slash cascade", used);
        emit log_named_uint("finalize 128x32, 127 slashed below min + deactivated", used);
        assertLt(used, GAS_CAP, "slash cascade finalize within budget");
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
        (address[] memory left,) = st.activeSet();
        emit log_named_uint("active members after", left.length);
        assertLt(left.length, st.MIN_ROUND_MEMBERS(), "registry collapsed");
        OnyxRound.RoundConfig memory c = _cfg();
        c.commitClose = uint64(block.number) + 100;
        c.revealClose = c.commitClose + 100;
        c.beaconHeight = c.revealClose + 1;
        c.validation.verdictCommitClose = c.beaconHeight + 100;
        c.validation.verdictRevealClose = c.validation.verdictCommitClose + 100;
        c.validation.canaryOpenClose = c.validation.verdictRevealClose + 50;
        c.validation.pendingClose = c.validation.canaryOpenClose + 150;
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = keccak256("unused");
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        vm.expectRevert(OnyxLedger.TooFewMembers.selector);
        st.openRound(ID + 1, c, ids, inits);
    }

    function test_L35_F3_FinalizeWorstCase_SlashCascade_Rate3() public {
        _fill();
        _bound32(3e4);
        uint256 g = gasleft();
        st.finalizeShard(ID, shard);
        uint256 used = g - gasleft();
        emit log_named_uint("finalize 128x32 rate 3%, slash cascade", used);
        assertLt(used, GAS_CAP);
    }

    function test_L35_F3_FinalizeWorstCase_SlashCascade_Rate10() public {
        _fill();
        _bound32(1e5);
        uint256 g = gasleft();
        st.finalizeShard(ID, shard);
        uint256 used = g - gasleft();
        emit log_named_uint("finalize 128x32 rate 10%, slash cascade", used);
        assertLt(used, GAS_CAP);
    }

    // ---------------------------------------------------------------- unseeded round (F2)

    /// Seed never drawn, beacon lapsed, pendingClose reached. Every call order must end the same
    /// way: the round is voided and every bond (revealed or not) comes back (M6-L35-F2).
    function test_L35_F2_UnseededRoundSameOutcomeEitherOrder() public {
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mk2, m2, SALT2, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root); // m2 never reveals
        vm.roll(base + 600); // pendingClose; beacon (base+201) lapsed at base+458
        uint256 snap = vm.snapshotState();

        st.voidShard(ID, shard); // voids the round and refunds this shard
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.drawSeed(ID);
        sx().reclaimBudget(ID);
        _solventHere();
        (uint256 m1A, uint256 m2A, uint256 burnedA) =
            (st.balanceOf(m1), st.balanceOf(m2), st.burned());

        vm.revertToState(snap);
        st.drawSeed(ID); // voids the round
        sx().refundVoidedShard(ID, shard);
        sx().reclaimBudget(ID);
        _solventHere();
        assertEq(st.balanceOf(m1), m1A, "m1 same either order");
        assertEq(st.balanceOf(m2), m2A, "m2 same either order");
        assertEq(st.burned(), burnedA, "burned same either order");
        assertEq(m2A, BOND, "the unrevealed bond comes back on a voided round");

        vm.revertToState(snap);
        st.drawSeed(ID);
        st.voidShard(ID, shard); // on a voided round voidShard is refundVoidedShard
        assertEq(st.balanceOf(m2), m2A, "voidShard after the round void refunds too");
    }

    /// The beacon is still in range at pendingClose but nobody drew it: a seed now would be
    /// useless (nothing can settle), so drawSeed voids the round instead, and the outcome again
    /// matches voidShard-first.
    function test_L35_F2_DrawSeedAtPendingCloseVoids() public {
        OnyxRound.RoundConfig memory c = _cfg();
        c.validation.verdictCommitClose =
            c.beaconHeight + st.MIN_VERDICT_WINDOW() + st.MIN_DRAW_SLACK();
        c.validation.verdictRevealClose = c.validation.verdictCommitClose + 20;
        c.validation.canaryOpenClose = c.validation.verdictRevealClose;
        c.validation.pendingClose = c.validation.canaryOpenClose + st.MIN_PENDING_GAP();
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        st.openRound(ID, c, ids, inits);
        _commit(mk1, m1, SALT1, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        vm.roll(c.validation.pendingClose);
        assertLt(block.number - c.beaconHeight, st.BEACON_WINDOW(), "beacon still in range");
        st.drawSeed(ID);
        assertEq(st.seedOf(ID), bytes32(0), "no useless seed");
        sx().refundVoidedShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Voided));
        _solventHere();
    }

    /// M6-L36-F6: once verdict commits have closed, a late seed would leave no verdict window,
    /// so drawing it voids the round instead; one block earlier it still draws.
    function _openShortVerdict() internal returns (OnyxRound.RoundConfig memory c) {
        c = _cfg();
        c.validation.verdictCommitClose =
            c.beaconHeight + st.MIN_VERDICT_WINDOW() + st.MIN_DRAW_SLACK();
        c.validation.verdictRevealClose = c.validation.verdictCommitClose + 20;
        c.validation.canaryOpenClose = c.validation.verdictRevealClose;
        c.validation.pendingClose = c.validation.canaryOpenClose + st.MIN_PENDING_GAP();
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        st.openRound(ID, c, ids, inits);
        _commit(mk1, m1, SALT1, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
    }

    function test_L36_F6_SeedAtVerdictCommitCloseVoids() public {
        OnyxRound.RoundConfig memory c = _openShortVerdict();
        vm.roll(c.validation.verdictCommitClose);
        assertLt(block.number, c.validation.pendingClose, "before pendingClose");
        st.drawSeed(ID);
        assertEq(st.seedOf(ID), bytes32(0), "no verdictless seed");
        sx().refundVoidedShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Voided));
        _solventHere();
    }

    /// M6-L37-F2: a seed drawn one block before verdict commits close left a 1-block verdict
    /// window. Now it voids unless `MIN_VERDICT_WINDOW` blocks remain.
    function test_L37_F2_SeedOneBlockBeforeVerdictCommitCloseVoids() public {
        OnyxRound.RoundConfig memory c = _openShortVerdict();
        vm.roll(c.validation.verdictCommitClose - 1);
        st.drawSeed(ID);
        assertEq(st.seedOf(ID), bytes32(0), "no 1-block verdict window");
        sx().refundVoidedShard(ID, shard);
        _solventHere();
    }

    function test_L37_F2_SeedInsideMinVerdictWindowVoids() public {
        OnyxRound.RoundConfig memory c = _openShortVerdict();
        vm.roll(c.validation.verdictCommitClose - st.MIN_VERDICT_WINDOW() + 1);
        st.drawSeed(ID);
        assertEq(st.seedOf(ID), bytes32(0), "window shorter than MIN_VERDICT_WINDOW");
    }

    /// The tightest valid config (verdictCommitClose = beaconHeight + MIN_VERDICT_WINDOW +
    /// MIN_DRAW_SLACK) can draw the seed at every one of its first MIN_DRAW_SLACK blocks, each
    /// leaving a full verdict window (M6-L44-F3).
    function test_L37_F2_SeedWithFullMinVerdictWindowDraws() public {
        OnyxRound.RoundConfig memory c = _openShortVerdict();
        uint256 snap = vm.snapshotState();
        vm.roll(c.beaconHeight + 1);
        st.drawSeed(ID);
        assertTrue(st.seedOf(ID) != bytes32(0), "first block draws");
        vm.revertToState(snap);
        vm.roll(c.validation.verdictCommitClose - st.MIN_VERDICT_WINDOW());
        assertEq(block.number, c.beaconHeight + st.MIN_DRAW_SLACK(), "slack blocks");
        st.drawSeed(ID);
        assertTrue(st.seedOf(ID) != bytes32(0), "last of MIN_DRAW_SLACK blocks draws");
    }

    /// M6-L44-F3: a config whose seed could be drawn in fewer than MIN_DRAW_SLACK blocks is
    /// refused (the old tightest config left a single block).
    function test_L45_F3_ConfigWithoutDrawSlackRefused() public {
        OnyxRound.RoundConfig memory c = _cfg();
        c.validation.verdictCommitClose =
            c.beaconHeight + st.MIN_VERDICT_WINDOW() + st.MIN_DRAW_SLACK() - 1;
        c.validation.verdictRevealClose = c.validation.verdictCommitClose + 20;
        c.validation.canaryOpenClose = c.validation.verdictRevealClose;
        c.validation.pendingClose = c.validation.canaryOpenClose + st.MIN_PENDING_GAP();
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        vm.expectRevert(OnyxLedger.BadConfig.selector);
        st.openRound(ID, c, ids, inits);
        c.validation.verdictCommitClose += 1;
        c.validation.verdictRevealClose = c.validation.verdictCommitClose + 20;
        c.validation.canaryOpenClose = c.validation.verdictRevealClose;
        c.validation.pendingClose = c.validation.canaryOpenClose + st.MIN_PENDING_GAP();
        st.openRound(ID, c, ids, inits);
    }

    /// M6-L44-F3: Rust (`seed_draw_voids`) and the contract (`drawSeed`) agree on when drawing
    /// the seed voids the round. Cases from fixtures/settlement/vectors.json `seedDraw`
    /// (heights are offsets from the beacon height).
    function test_L45_F3_SeedDrawVoidMatchesRust() public {
        string memory json = vm.readFile("../fixtures/settlement/vectors.json");
        assertEq(vm.parseJsonUint(json, ".seedDraw.minVerdictWindow"), st.MIN_VERDICT_WINDOW());
        assertEq(vm.parseJsonUint(json, ".seedDraw.minDrawSlack"), st.MIN_DRAW_SLACK());
        assertEq(vm.parseJsonUint(json, ".seedDraw.beaconWindow"), st.BEACON_WINDOW());
        uint256[] memory vccs = vm.parseJsonUintArray(json, ".seedDraw.vcc");
        uint256[] memory ats = vm.parseJsonUintArray(json, ".seedDraw.at");
        bool[] memory voidss = vm.parseJsonBoolArray(json, ".seedDraw.voids");
        uint256 n = vccs.length;
        assertEq(ats.length, n);
        assertEq(voidss.length, n);
        assertGt(n, 0, "cases");
        for (uint256 i = 0; i < n; i++) {
            uint64 vcc = uint64(vccs[i]);
            uint64 at = uint64(ats[i]);
            bool voids = voidss[i];
            uint256 snap = vm.snapshotState();
            OnyxRound.RoundConfig memory c = _cfg();
            c.validation.verdictCommitClose = c.beaconHeight + vcc;
            c.validation.verdictRevealClose = c.validation.verdictCommitClose + 20;
            c.validation.canaryOpenClose = c.validation.verdictRevealClose;
            c.validation.pendingClose = c.validation.canaryOpenClose + st.MIN_PENDING_GAP();
            bytes32[] memory ids = new bytes32[](1);
            ids[0] = shard;
            OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
            inits[0] = OnyxLedger.ShardInit(
                OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
            );
            st.openRound(ID, c, ids, inits);
            vm.roll(c.beaconHeight + at);
            st.drawSeed(ID);
            assertEq(st.seedOf(ID) == bytes32(0), voids, string.concat("case ", vm.toString(i)));
            vm.revertToState(snap);
        }
    }

    function _solventHere() internal {
        _releaseIfFinalized();
        uint256 sum = st.burned() + st.rewardPool() + st.coordinatorBond() + _standbyLiabilities();
        address[6] memory w = [vals[0], vals[1], vals[2], m1, m2, address(this)];
        for (uint256 i = 0; i < 6; i++) {
            (uint128 sk,,,) = st.validators(w[i]);
            sum += sk + st.balanceOf(w[i]);
        }
        assertEq(usdc.balanceOf(address(st)), sum, "solvent");
    }

    // ---------------------------------------------------------------- round span (F1)

    /// The coordinator cannot lock stakes ~forever: pendingClose is capped at
    /// `MAX_ROUND_SPAN` blocks after opening, which bounds every other height too (M6-L35-F1).
    function test_L35_F1_RoundSpanIsCapped() public {
        OnyxRound.RoundConfig memory c = _cfg();
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        c.validation.pendingClose = type(uint64).max - 1;
        vm.expectRevert(OnyxLedger.BadConfig.selector);
        st.openRound(ID, c, ids, inits);
        c.validation.pendingClose = uint64(block.number) + st.MAX_ROUND_SPAN() + 1;
        vm.expectRevert(OnyxLedger.BadConfig.selector);
        st.openRound(ID, c, ids, inits);
        c.validation.pendingClose = uint64(block.number) + st.MAX_ROUND_SPAN();
        st.openRound(ID, c, ids, inits);
        vm.prank(vals[0]);
        sx().requestUnbond();
        (,, uint64 unbondAt,) = st.validators(vals[0]);
        assertLe(
            unbondAt,
            uint64(block.number) + st.MAX_ROUND_SPAN() + st.MAX_DISPUTE_TAIL(),
            "lock is bounded (round span plus dispute tail, ADR-0024 item 3)"
        );
    }

    /// ADR-0024 item 3: a dispute tail above `MAX_DISPUTE_TAIL` is refused.
    function test_DisputeTailIsCapped() public {
        OnyxRound.RoundConfig memory c = _cfg();
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        // 2 x 50,000 + 2 x (256 + 150) = 100,812 > 100,000.
        c.dispute.challengeWindow = 50_000;
        vm.expectRevert(OnyxLedger.BadConfig.selector);
        st.openRound(ID, c, ids, inits);
        c.dispute.challengeWindow = 49_000; // 98,812: fits
        st.openRound(ID, c, ids, inits);
    }
}
