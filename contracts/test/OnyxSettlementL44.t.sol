// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxLedger} from "../src/OnyxLedger.sol";
import {OnyxSettlementReviewTest} from "./OnyxSettlementReview.t.sol";
import {OnyxSettlement} from "../src/OnyxSettlement.sol";
import {OnyxRound} from "../src/OnyxRound.sol";

/// M6 loop 44: unrevealed verdict commits are slashed once each at round level, whatever the
/// shard's outcome, as Rust's round-level settlement does (M6-F7 / M6-L34-F7).
contract OnyxSettlementL44Test is OnyxSettlementReviewTest {
    bytes32 internal constant OTHER = keccak256("other");

    function _commitVR(address v, address miner, uint8 kind, bytes32 root) internal {
        bytes32 h = OnyxRound.verdictCommitHash(
            st.seedOf(ID), v, shard, miner, kind, root, _vsalt(v, miner)
        );
        vm.prank(v);
        st.commitVerdict(ID, shard, miner, h);
    }

    function _revealVR(address v, address miner, uint8 kind, bytes32 root) internal {
        vm.prank(v);
        st.revealVerdict(ID, shard, miner, kind, root, _vsalt(v, miner));
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

    function _stake(address v) internal view returns (uint128 s) {
        (s,,,) = st.validators(v);
    }

    /// vals[0] agrees on m1, vals[1] disagrees on m1 (shard goes Pending), vals[2] commits on
    /// m1 and m2 and reveals nothing.
    function _disputedRound() internal {
        _toVerdicts();
        _commitVR(vals[0], m1, OnyxRound.AGREE, bytes32(0));
        _commitVR(vals[1], m1, OnyxRound.DISAGREE, OTHER);
        _commitVR(vals[2], m1, OnyxRound.AGREE, bytes32(0));
        _commitVR(vals[2], m2, OnyxRound.AGREE, bytes32(0));
        vm.roll(base + 300);
        _revealVR(vals[0], m1, OnyxRound.AGREE, bytes32(0));
        _revealVR(vals[1], m1, OnyxRound.DISAGREE, OTHER);
        _openNoCanaries();
    }

    /// A Pending shard (disputed) still costs each unrevealed commit `validatorSlash` (was:
    /// only shards that finalized slashed).
    function test_L44_F7_PendingShardSlashesUnrevealed() public {
        _disputedRound();
        assertEq(st.unrevealedVerdicts(ID, vals[2]), 2);
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Pending));
        assertEq(_stake(vals[2]), STAKE - 4_000, "two unrevealed commits, one slash each");
        assertEq(_stake(vals[0]), STAKE, "agree: no slash");
        assertEq(_stake(vals[1]), STAKE, "revealed disagree: not slashed here");
        // Voided later: no second round-level slash.
        vm.roll(base + 600);
        st.voidShard(ID, shard);
        assertEq(_stake(vals[2]), STAKE - 4_000, "slashed once");
        _releaseIfFinalized();
        sx().reclaimBudget(ID);
        _solventHere();
    }

    /// `settleRound` applies the round-level slashes on its own, once; a later finalize does
    /// not repeat them.
    function test_L44_F7_SettleRoundOnceThenFinalize() public {
        _toVerdicts();
        _vote(OnyxRound.AGREE, bytes32(0), false);
        vm.roll(base + 300);
        for (uint256 i = 0; i < 2; i++) {
            _revealV(vals[i], m1, OnyxRound.AGREE);
            _revealV(vals[i], m2, OnyxRound.AGREE);
        }
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.settleRound(ID); // verdict reveals still open
        vm.roll(base + 400);
        st.settleRound(ID);
        assertEq(_stake(vals[2]), STAKE - 4_000);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.settleRound(ID);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        assertEq(_stake(vals[2]), STAKE - 4_000, "not slashed again by finalize");
        _releaseIfFinalized();
        sx().reclaimBudget(ID);
        _solventHere();
    }

    /// A validator who agreed on one reveal and left another commit unrevealed on the same
    /// shard earns its fee and loses one `validatorSlash` (Rust: fee per revealed Agree,
    /// round-level slash per unrevealed commit, no missed slash).
    function test_L44_F7_AgreeAndUnrevealedOnOneShard() public {
        _toVerdicts();
        for (uint256 i = 0; i < 3; i++) {
            _commitVR(vals[i], m1, OnyxRound.AGREE, bytes32(0));
        }
        _commitVR(vals[2], m2, OnyxRound.AGREE, bytes32(0));
        vm.roll(base + 300);
        for (uint256 i = 0; i < 3; i++) {
            _revealVR(vals[i], m1, OnyxRound.AGREE, bytes32(0));
        }
        _openNoCanaries();
        uint256 before = st.balanceOf(vals[2]);
        st.finalizeShard(ID, shard);
        assertEq(st.balanceOf(vals[2]) - before, 10, "fee for the revealed Agree");
        assertEq(_stake(vals[2]), STAKE - 2_000, "one unrevealed commit, no missed slash");
        assertEq(_stake(vals[0]), STAKE, "answered on the shard: not missed");
        _releaseIfFinalized();
        sx().reclaimBudget(ID);
        _solventHere();
    }

    /// Stakes now stay locked through the dispute tail (ADR-0024 item 3): `settleRound` and
    /// a void still slash after `pendingClose`, and nothing slashes once the lock has ended
    /// (base + 1876 here), when stakes may have been withdrawn.
    function test_L44_F7_NoRoundLevelSlashAfterLocksEnd() public {
        _disputedRound();
        vm.roll(base + 1876);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.settleRound(ID);
        st.voidShard(ID, shard);
        assertEq(_stake(vals[2]), STAKE, "no late slash");
    }

    function test_L44_F1_SettleRoundAfterPendingCloseWhileLocked() public {
        _disputedRound();
        vm.roll(base + 600);
        st.settleRound(ID);
        assertEq(_stake(vals[2]), STAKE - 2 * 2_000, "two unrevealed commits, slashed once each");
    }

    function test_L44_F7_SettleRoundRefusedOnVoidedRound() public {
        _open();
        _commit(mk1, m1, SALT1, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        vm.roll(base + 300); // past verdictCommitClose: the seed voids
        st.drawSeed(ID);
        vm.roll(base + 400);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.settleRound(ID);
    }

    // ------------------------------------------------------------ Rust vector (M6-F7)

    address[] internal roles;

    /// Replay a scenario from fixtures/settlement/vectors.json (written by Rust's settle path,
    /// adapters/source-btc/tests/evm_vectors.rs): role `i` does `actions[2i + j]` on miner `j`
    /// (0 silent, 1 agree, 2 disagree, 3 abstain, 4 commit and never reveal).
    function _replay(string memory key) internal returns (string memory json, string memory p) {
        json = vm.readFile("../fixtures/settlement/vectors.json");
        p = string.concat(".unrevealedSlashing.", key);
        uint256[] memory act = vm.parseJsonUintArray(json, string.concat(p, ".actions"));
        uint256 n = act.length / 2;
        for (uint256 i = 0; i < n; i++) {
            roles.push(i < 3 ? vals[i] : vm.addr(0xC100 + i));
            if (i >= 3) {
                _fund(roles[i], STAKE);
                vm.prank(roles[i]);
                st.stake(STAKE);
            }
        }
        _toVerdicts();
        address[2] memory ms = [m1, m2];
        for (uint256 i = 0; i < n; i++) {
            for (uint256 j = 0; j < 2; j++) {
                uint256 a = act[2 * i + j];
                if (a == 0) continue;
                (uint8 kind, bytes32 root) = a == 2
                    ? (OnyxRound.DISAGREE, OTHER)
                    : a == 3 ? (OnyxRound.ABSTAIN, bytes32(0)) : (OnyxRound.AGREE, bytes32(0));
                _commitVR(roles[i], ms[j], kind, root);
            }
        }
        vm.roll(base + 300);
        for (uint256 i = 0; i < n; i++) {
            for (uint256 j = 0; j < 2; j++) {
                uint256 a = act[2 * i + j];
                if (a == 0 || a == 4) continue;
                (uint8 kind, bytes32 root) = a == 2
                    ? (OnyxRound.DISAGREE, OTHER)
                    : a == 3 ? (OnyxRound.ABSTAIN, bytes32(0)) : (OnyxRound.AGREE, bytes32(0));
                _revealVR(roles[i], ms[j], kind, root);
            }
        }
        _openNoCanaries();
    }

    /// Unanimous shard: fee, missed and unrevealed slashes per role equal Rust's.
    function test_L44_F7_UnrevealedSlashingMatchesRust_Finalized() public {
        (string memory json, string memory p) = _replay("finalized");
        uint256[] memory fee = vm.parseJsonUintArray(json, string.concat(p, ".fee"));
        uint256[] memory missed = vm.parseJsonUintArray(json, string.concat(p, ".missed"));
        uint256[] memory unrev = vm.parseJsonUintArray(json, string.concat(p, ".unrevealed"));
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
        for (uint256 i = 0; i < roles.length; i++) {
            assertEq(st.balanceOf(roles[i]), fee[i], "fee");
            assertEq(STAKE - _stake(roles[i]), missed[i] + unrev[i], "missed + unrevealed");
            assertEq(st.unrevealedVerdicts(ID, roles[i]) * 2_000, unrev[i], "unrevealed");
        }
    }

    /// Disputed shard (Rust reruns it, the contract leaves it Pending until the dispute game):
    /// the unrevealed slashes still equal Rust's, and nothing else is applied here.
    function test_L44_F7_UnrevealedSlashingMatchesRust_Disputed() public {
        (string memory json, string memory p) = _replay("disputed");
        uint256[] memory unrev = vm.parseJsonUintArray(json, string.concat(p, ".unrevealed"));
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Pending));
        for (uint256 i = 0; i < roles.length; i++) {
            assertEq(STAKE - _stake(roles[i]), unrev[i], "unrevealed");
        }
    }

    /// Worst case for the round-level loop: 128 members, each with an unrevealed commit on
    /// every one of 32 reveals; one member agrees so the shard finalizes. Every member is
    /// slashed below the minimum and leaves the set, all inside one finalize.
    function test_L44_F7_FinalizeGasAtBound_AllUnrevealed() public {
        _fillValidators();
        _openCfg(_cfg());
        address[] memory ms = _commitRevealAll();
        vm.roll(base + 202);
        st.drawSeed(ID);
        address[] memory mem = st.roundMembers(ID);
        // Every member but the standbys (the dispute panel pool, L29-F4) commits on every reveal.
        for (uint256 i = 0; i < mem.length; i++) {
            if (_isStandby(mem[i])) continue;
            for (uint256 j = 0; j < ms.length; j++) {
                _commitV(mem[i], ms[j], OnyxRound.AGREE);
            }
        }
        vm.roll(base + 300);
        uint256 k = 0;
        while (_isStandby(mem[k])) k++;
        _revealV(mem[k], ms[0], OnyxRound.AGREE);
        _openNoCanaries();
        uint256 g = gasleft();
        st.finalizeShard(ID, shard);
        uint256 used = g - gasleft();
        emit log_named_uint("finalize gas 128x32, every member unrevealed", used);
        assertLt(used, GAS_BUDGET, "finalize within budget at the bound");
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
        uint256 k2 = k + 1;
        while (_isStandby(mem[k2])) k2++;
        assertEq(_stake(mem[k2]), 0, "32 unrevealed commits: capped at the stake");
        assertEq(st.unrevealedVerdicts(ID, mem[k]), ms.length - 1);
    }

    /// Round-level slash cascade: all 128 members leave 3 commits unrevealed, are slashed below
    /// the minimum and deactivated in one call. Its cost plus the shard-level cascade
    /// (`test_L35_F3_FinalizeWorstCase_SlashCascade_*`, which already pays the round-level
    /// loop) bounds the first finalize of a round, under the 15M budget.
    function test_L44_F7_SettleRoundGasAtBound_Cascade() public {
        _fillValidators();
        _openCfg(_cfg());
        address[] memory ms = _commitRevealAll();
        vm.roll(base + 202);
        st.drawSeed(ID);
        address[] memory mem = st.roundMembers(ID);
        for (uint256 i = 0; i < mem.length; i++) {
            for (uint256 j = 0; j < 3; j++) {
                _commitV(mem[i], ms[j], OnyxRound.AGREE);
            }
        }
        vm.roll(base + 400);
        uint256 g = gasleft();
        st.settleRound(ID);
        uint256 used = g - gasleft();
        emit log_named_uint("settleRound gas, 128 members slashed below min", used);
        (address[] memory left,) = st.activeSet();
        assertEq(left.length, 0, "every member deactivated");
        assertLt(used + 7_200_000, GAS_BUDGET, "with the worst shard-level cascade");
    }
}
