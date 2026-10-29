// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxLedger} from "../src/OnyxLedger.sol";
import {OnyxSettlementTest} from "./OnyxSettlement.t.sol";
import {OnyxSettlement} from "../src/OnyxSettlement.sol";
import {OnyxRound} from "../src/OnyxRound.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";
import {OnyxSig} from "../src/OnyxSig.sol";

/// Regression tests for the M6 loop 34 review (M6-L34-F1..F5), built from the reviewer's
/// proofs of concept: each asserted the bug and failed after its fix; they now assert the
/// fixed behavior. Also the escrow solvency fuzz the reviewer wrote, kept as a permanent test.
contract OnyxSettlementReviewTest is OnyxSettlementTest {
    /// Per-transaction gas cap used for every bound (EIP-7825 is 16.7M); kept below with margin.
    uint256 internal constant GAS_BUDGET = 15_000_000;

    address[] internal who;

    function _openCfg(OnyxRound.RoundConfig memory c) internal {
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        st.openRound(ID, c, ids, inits);
    }

    function _commitV(address v, address miner, uint8 kind) internal {
        bytes32 h = OnyxRound.verdictCommitHash(
            st.seedOf(ID), v, shard, miner, kind, bytes32(0), _vsalt(v, miner)
        );
        vm.prank(v);
        st.commitVerdict(ID, shard, miner, h);
    }

    function _revealV(address v, address miner, uint8 kind) internal {
        vm.prank(v);
        st.revealVerdict(ID, shard, miner, kind, bytes32(0), _vsalt(v, miner));
    }

    /// One miner reveals; vals[0..1] agree; vals[2] commits and never reveals.
    function _oneLazyValidator(OnyxRound.RoundConfig memory c) internal {
        _openCfg(c);
        _commit(mk1, m1, SALT1, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        vm.roll(base + 202);
        st.drawSeed(ID);
        for (uint256 i = 0; i < 3; i++) {
            _commitV(vals[i], m1, OnyxRound.AGREE);
        }
        vm.roll(base + 300);
        _revealV(vals[0], m1, OnyxRound.AGREE);
        _revealV(vals[1], m1, OnyxRound.AGREE);
    }

    // ------------------------------------------------------------------ F2

    /// M6-L34-F2: a validator slashed below the minimum leaves the set and starts normal
    /// unbonding, so what is left can be withdrawn after the delay (was stuck forever).
    function test_L34_F2_SlashedBelowMinStakeUnbonds() public {
        OnyxRound.RoundConfig memory c = _cfg();
        c.validation.validatorSlash = 4_500;
        _oneLazyValidator(c);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        (uint128 s2,, uint64 unbondAt, bool active) = st.validators(vals[2]);
        assertEq(s2, 500);
        assertFalse(active);
        assertGe(unbondAt, c.validation.pendingClose, "never before the round lock ends");
        vm.roll(unbondAt);
        vm.prank(vals[2]);
        sx().withdrawStake();
        assertEq(st.balanceOf(vals[2]), 500);
    }

    // ------------------------------------------------------------------ F3

    /// M6-L34-F3: settling is closed at `pendingClose`, when stakes unlock, so a slash can no
    /// longer be escaped by withdrawing first and settling late. The shard is voided instead:
    /// nobody is slashed or paid.
    /// Settling closes at `pendingClose`, but stakes stay locked through the dispute tail, so
    /// a void there still applies the round-level slash nobody applied (M6-L44-F1, ADR-0024
    /// item 3; it was dropped before).
    function test_L34_F3_NoSettleAfterPendingCloseButVoidStillSlashes() public {
        _oneLazyValidator(_cfg());
        vm.prank(vals[2]);
        sx().requestUnbond();
        _openNoCanaries();
        vm.roll(base + 600); // pendingClose
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.finalizeShard(ID, shard);
        vm.prank(vals[2]);
        vm.expectRevert(OnyxLedger.Locked.selector);
        sx().withdrawStake();
        st.voidShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Voided));
        assertEq(st.balanceOf(m1), BOND, "revealed bond returned");
        assertEq(st.burned(), 2_000, "the unrevealed verdict is slashed on the void");
        assertEq(_stakeOf(vals[2]), STAKE - 2_000);
    }

    function _stakeOf(address v) internal view returns (uint128 s) {
        (s,,,) = st.validators(v);
    }

    /// M6-L34-F3: a settle before `pendingClose` slashes a stake that is still locked.
    function test_L34_F3_SlashLandsWhileLocked() public {
        _oneLazyValidator(_cfg());
        vm.prank(vals[2]);
        sx().requestUnbond();
        _openNoCanaries();
        vm.prank(vals[2]);
        vm.expectRevert(OnyxLedger.Locked.selector);
        sx().withdrawStake();
        st.finalizeShard(ID, shard);
        (uint128 s2,,,) = st.validators(vals[2]);
        assertEq(s2, STAKE - 2_000, "validatorSlash applied");
    }

    /// M6-L34-F3: the canary deadline cannot be called once the coordinator bond unlocks,
    /// and the first settle after the deadline applies it itself.
    function test_L34_F3_CanaryMissAppliedBySettleAndOnlyBeforeLocksEnd() public {
        _oneLazyValidator(_cfg());
        vm.roll(base + 450);
        uint128 before = st.coordinatorBond();
        st.finalizeShard(ID, shard); // canaries unopened: settle applies the missed deadline
        assertEq(before - st.coordinatorBond(), 500);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        sx().missCanaryDeadline(ID); // already resolved
    }

    function test_L34_F3_CanaryMissRefusedAfterPendingClose() public {
        _oneLazyValidator(_cfg());
        vm.roll(base + 600);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        sx().missCanaryDeadline(ID);
    }

    // ------------------------------------------------------------------ F4

    /// M6-L34-F4: with the beacon lapsed and no seed, the missed canary deadline voids the
    /// round instead of slashing the coordinator on a dead round.
    function test_L34_F4_CanaryMissOnLapsedBeaconVoidsWithoutSlash() public {
        OnyxRound.RoundConfig memory c = _cfg();
        c.validation.canaryOpenClose = base + 460; // beacon base+201, +256 = base+457
        _openCfg(c);
        vm.roll(base + 460);
        uint128 bondBefore = st.coordinatorBond();
        sx().missCanaryDeadline(ID);
        assertEq(st.coordinatorBond(), bondBefore, "no slash on a voided round");
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.drawSeed(ID); // already voided
    }

    /// M6-L34-F4: before the beacon lapses, the deadline needs a drawn seed.
    function test_L34_F4_CanaryMissNeedsSeed() public {
        _open();
        vm.roll(base + 450); // beacon base+201: 249 blocks, still drawable
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        sx().missCanaryDeadline(ID);
    }

    // ------------------------------------------------------------------ F1 (gas bounds)

    function _fillValidators() internal {
        uint256 nV = st.MAX_VALIDATORS();
        for (uint256 i = 3 + standbyList.length; i < nV; i++) {
            address a = vm.addr(0xD000 + i);
            _fund(a, STAKE);
            vm.prank(a);
            st.stake(STAKE);
        }
    }

    /// 32 miners commit and reveal the same root on the shard.
    function _commitRevealAll() internal returns (address[] memory ms) {
        bytes32 tag = st.roundTag(ID);
        uint256 nM = st.MAX_COMMITS_PER_SHARD();
        ms = new address[](nM);
        for (uint256 j = 0; j < nM; j++) {
            ms[j] = vm.addr(0xE000 + j);
            _fund(ms[j], BOND);
            bytes32 h = _commitHash(ms[j], keccak256(abi.encodePacked("s", j)), fields.root);
            (uint8 v, bytes32 r, bytes32 sg) = vm.sign(
                0xE000 + j, OnyxSig.eip191Digest(OnyxRound.commitMessage(tag, shard, h, BOND))
            );
            st.commit(ID, shard, ms[j], h, BOND, abi.encodePacked(r, sg, v));
        }
        vm.roll(base + 100);
        for (uint256 j = 0; j < nM; j++) {
            _reveal(ms[j], keccak256(abi.encodePacked("s", j)), fields.root);
        }
    }

    /// Every assigned (member, miner) pair commits Agree, then reveals it; the standbys stay
    /// silent, so a dispute panel can still be drawn (L29-F4).
    function _everyoneAgrees(address[] memory ms, uint64 rate) internal {
        address[] memory mem = st.roundMembers(ID);
        bytes32 seed = st.seedOf(ID);
        for (uint256 i = 0; i < mem.length; i++) {
            if (_isStandby(mem[i])) continue;
            for (uint256 j = 0; j < ms.length; j++) {
                if (OnyxRound.assigned(seed, mem[i], shard, ms[j], rate)) {
                    _commitV(mem[i], ms[j], OnyxRound.AGREE);
                }
            }
        }
        vm.roll(base + 300);
        for (uint256 i = 0; i < mem.length; i++) {
            if (_isStandby(mem[i])) continue;
            for (uint256 j = 0; j < ms.length; j++) {
                if (OnyxRound.assigned(seed, mem[i], shard, ms[j], rate)) {
                    _revealV(mem[i], ms[j], OnyxRound.AGREE);
                }
            }
        }
    }

    /// The first assigned (member, miner) pair agrees; everyone else stays silent.
    function _oneAgrees(address[] memory ms, uint64 rate) internal {
        address[] memory mem = st.roundMembers(ID);
        bytes32 seed = st.seedOf(ID);
        (uint256 i0, uint256 j0) = (0, 0);
        while (!OnyxRound.assigned(seed, mem[i0], shard, ms[j0], rate)) {
            j0++;
            if (j0 == ms.length) (i0, j0) = (i0 + 1, 0);
        }
        _commitV(mem[i0], ms[j0], OnyxRound.AGREE);
        vm.roll(base + 300);
        _revealV(mem[i0], ms[j0], OnyxRound.AGREE);
    }

    /// Fill one shard with 32 revealed commits under 128 validators. `vote` = every
    /// assigned validator agrees; otherwise one agrees and the rest stay silent.
    function _boundRound(bool vote, uint64 rate) internal returns (address[] memory ms) {
        _fillValidators();
        uint256 g0 = gasleft();
        {
            OnyxRound.RoundConfig memory c = _cfg();
            c.validation.ratePpm = rate;
            c.validation.minerReward = 100;
            _openCfg(c);
        }
        uint256 openGas = g0 - gasleft();
        emit log_named_uint("openRound gas (128 members, 1 shard)", openGas);
        assertLt(openGas, GAS_BUDGET, "openRound within budget");
        ms = _commitRevealAll();
        vm.roll(base + 202);
        st.drawSeed(ID);
        if (vote) _everyoneAgrees(ms, rate);
        else _oneAgrees(ms, rate);
        _openNoCanaries();
    }

    function _finalizeGas(bool vote, uint64 rate) internal returns (uint256 used) {
        _boundRound(vote, rate);
        uint256 g = gasleft();
        st.finalizeShard(ID, shard);
        used = g - gasleft();
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
        assertLt(used, GAS_BUDGET, "finalize within budget at the bound");
    }

    /// M6-L34-F1: settling at the bound (128 validators x 32 reveals) fits one transaction.
    /// Was 38.1M at 100% with everyone agreeing.
    function test_L34_F1_FinalizeGasAtBound_AllAgree() public {
        emit log_named_uint("finalize gas 128x32 rate 100% all agree", _finalizeGas(true, 1e6));
    }

    function test_L34_F1_FinalizeGasAtBound_MostlySilent() public {
        emit log_named_uint("finalize gas 128x32 rate 100% one agree", _finalizeGas(false, 1e6));
    }

    function test_L34_F1_FinalizeGasAtBound_Rate10pctSilent() public {
        // Low rate, almost everyone silent: every member needs the assignment scan.
        emit log_named_uint("finalize gas 128x32 rate 10% one agree", _finalizeGas(false, 1e5));
    }

    /// M6-L34-F1: a shard nobody settled (or nobody could) is voided after `pendingClose`
    /// within budget, and the round budget can be reclaimed: funds never lock.
    function test_L34_F1_OpenShardVoidsAfterPendingCloseWithinBudget() public {
        address[] memory ms = _boundRound(true, 1e6);
        vm.roll(base + 600);
        uint256 g = gasleft();
        st.voidShard(ID, shard);
        uint256 used = g - gasleft();
        emit log_named_uint("voidShard gas, 32 reveals", used);
        assertLt(used, GAS_BUDGET);
        assertEq(st.balanceOf(ms[0]), BOND);
        uint256 poolBefore = st.rewardPool();
        sx().reclaimBudget(ID);
        assertGt(st.rewardPool(), poolBefore, "budget back in the pool");
    }

    /// M6-L34-F5: opening a round at the bound (128 members x 64 shards) fits the budget.
    function test_L34_F5_OpenRoundGasAtBound() public {
        _fillValidators();
        uint256 n = st.MAX_SHARDS();
        bytes32[] memory ids = new bytes32[](n);
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](n);
        bytes32 fh = OnyxCommit.shardFieldsHash(fields);
        for (uint256 k = 0; k < n; k++) {
            bytes32 ph = keccak256(abi.encodePacked("params", k));
            ids[k] = OnyxCommit.shardId(fh, ph, keccak256("registry"));
            inits[k] = OnyxLedger.ShardInit(fh, ph, keccak256("registry"));
        }
        for (uint256 i = 1; i < n; i++) {
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
        emit log_named_uint("openRound gas 128 members x 64 shards", used);
        assertLt(used, GAS_BUDGET);
    }

    /// The registry snapshot is ascending by address whatever the staking order (heapsort,
    /// M6-L34-F5), and its root is the one written into the round.
    function testFuzz_ActiveSetSortedAndRootMatches(uint256 seed, uint8 count) public {
        uint256 n = bound(count, 0, 60);
        for (uint256 i = 0; i < n; i++) {
            address a = vm.addr(uint256(keccak256(abi.encodePacked(seed, i))) % 1e70 + 1);
            (uint128 had,,,) = st.validators(a);
            if (had != 0) continue;
            _fund(a, STAKE + i);
            vm.prank(a);
            st.stake(uint128(STAKE + i));
        }
        (address[] memory m, uint128[] memory b) = st.activeSet();
        for (uint256 i = 1; i < m.length; i++) {
            assertLt(uint160(m[i - 1]), uint160(m[i]), "ascending");
        }
        _open();
        assertEq(sx().roundSetRoot(ID), OnyxRound.validatorSetRoot(m, b));
    }

    // ------------------------------------------------------------------ solvency

    function _who() internal {
        for (uint256 i = 0; i < 3; i++) {
            who.push(vals[i]);
        }
        who.push(m1);
        who.push(m2);
        who.push(address(this));
    }

    /// After every shard is terminal and the budget reclaimed, the token balance equals
    /// burned + pool + coordinator bond + every stake + every credit.
    function _solvent() internal {
        _releaseIfFinalized();
        sx().reclaimBudget(ID);
        uint256 sum = st.burned() + st.rewardPool() + st.coordinatorBond() + _standbyLiabilities();
        for (uint256 i = 0; i < who.length; i++) {
            (uint128 s,,,) = st.validators(who[i]);
            sum += s + st.balanceOf(who[i]);
        }
        assertEq(usdc.balanceOf(address(st)), sum, "escrow == liabilities + burned");
    }

    function testFuzz_SolventAfterSettleOrVoid(
        uint8 revealMask,
        uint8 voteMask,
        bool canaryMiss,
        bool settle
    ) public {
        _who();
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mk2, m2, SALT2, fields.root);
        vm.roll(base + 100);
        if (revealMask & 1 != 0) _reveal(m1, SALT1, fields.root);
        if (revealMask & 2 != 0) _reveal(m2, SALT2, fields.root);
        vm.roll(base + 202);
        st.drawSeed(ID);
        address[2] memory ms = [m1, m2];
        for (uint256 i = 0; i < 3; i++) {
            for (uint256 j = 0; j < 2; j++) {
                if ((revealMask >> j) & 1 == 0) continue;
                if ((voteMask >> (i * 2 + j)) & 1 != 0) _commitV(vals[i], ms[j], OnyxRound.AGREE);
            }
        }
        vm.roll(base + 300);
        for (uint256 i = 0; i < 3; i++) {
            for (uint256 j = 0; j < 2; j++) {
                if ((revealMask >> j) & 1 == 0) continue;
                if ((voteMask >> (i * 2 + j)) & 1 == 0) continue;
                if ((revealMask >> (2 + i)) & 1 != 0) _revealV(vals[i], ms[j], OnyxRound.AGREE);
            }
        }
        if (canaryMiss) {
            vm.roll(base + 450);
        } else {
            _openNoCanaries();
        }
        if (settle) st.finalizeShard(ID, shard);
        if (st.shardStatus(ID, shard) != OnyxLedger.ShardStatus.Finalized) {
            if (st.shardStatus(ID, shard) != OnyxLedger.ShardStatus.Empty) {
                vm.roll(base + 600);
                st.voidShard(ID, shard);
            }
        }
        _solvent();
    }

    function test_SolventAfterVoidedRound() public {
        _who();
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mk2, m2, SALT2, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        vm.roll(base + 201 + 257);
        st.drawSeed(ID);
        sx().refundVoidedShard(ID, shard);
        _solvent();
    }
}
