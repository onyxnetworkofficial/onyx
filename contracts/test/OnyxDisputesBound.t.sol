// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxSettlementFixture} from "./OnyxSettlement.t.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";
import {OnyxDisputes} from "../src/OnyxDisputes.sol";
import {OnyxLedger} from "../src/OnyxLedger.sol";
import {OnyxRound} from "../src/OnyxRound.sol";
import {OnyxSig} from "../src/OnyxSig.sol";

/// The dispute game at the contract's bounds (128 validators, 32 reveals per shard): every
/// call a case needs stays within the per-transaction budget (ADR-0024 item 8).
contract OnyxDisputesBoundTest is OnyxSettlementFixture {
    uint256 internal constant GAS_CAP = 15_000_000;
    bytes32 internal constant OTHER = keccak256("other root");

    /// `_fill` sizes the registry itself.
    function _standbys() internal pure override returns (uint256) {
        return 0;
    }

    function _fill(uint256 to) internal {
        for (uint256 i = 3; i < to; i++) {
            address a = vm.addr(0xD000 + i);
            _fund(a, STAKE);
            vm.prank(a);
            st.stake(STAKE);
        }
    }

    function _openWith(OnyxRound.RoundConfig memory c) internal {
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        st.openRound(ID, c, ids, inits);
    }

    /// `n` miners commit and reveal the honest root; the seed is drawn.
    function _miners(uint256 n) internal returns (address[] memory ms) {
        bytes32 tag = st.roundTag(ID);
        ms = new address[](n);
        for (uint256 j = 0; j < n; j++) {
            ms[j] = vm.addr(0xE000 + j);
            _fund(ms[j], BOND);
            bytes32 hh = _commitHash(ms[j], keccak256(abi.encodePacked("s", j)), fields.root);
            (uint8 v, bytes32 r, bytes32 sg) = vm.sign(
                0xE000 + j, OnyxSig.eip191Digest(OnyxRound.commitMessage(tag, shard, hh, BOND))
            );
            st.commit(ID, shard, ms[j], hh, BOND, abi.encodePacked(r, sg, v));
        }
        vm.roll(base + 100);
        for (uint256 j = 0; j < n; j++) {
            _reveal(ms[j], keccak256(abi.encodePacked("s", j)), fields.root);
        }
        vm.roll(base + 202);
        st.drawSeed(ID);
    }

    /// Worst case for judging: all 128 members gave a wrong Disagree on all 32 reveals (4,096
    /// verdicts, each slashed). Nobody is left to draw, so the coordinator's rerun decides;
    /// `settleCase` runs in batches of 8 members.
    function test_SettleCaseAtTheBound() public {
        vm.pauseGasMetering(); // 8,192 verdict calls of setup would exceed the test's gas
        _fill(128);
        _openWith(_cfg());
        address[] memory ms = _miners(32);
        address[] memory mem = st.roundMembers(ID);
        bytes32 seed = st.seedOf(ID);
        for (uint256 i = 0; i < mem.length; i++) {
            for (uint256 j = 0; j < ms.length; j++) {
                bytes32 h = OnyxRound.verdictCommitHash(
                    seed, mem[i], shard, ms[j], OnyxRound.DISAGREE, OTHER, _vsalt(mem[i], ms[j])
                );
                vm.prank(mem[i]);
                st.commitVerdict(ID, shard, ms[j], h);
            }
        }
        vm.roll(base + 300);
        for (uint256 i = 0; i < mem.length; i++) {
            for (uint256 j = 0; j < ms.length; j++) {
                vm.prank(mem[i]);
                st.revealVerdict(ID, shard, ms[j], OnyxRound.DISAGREE, OTHER, _vsalt(mem[i], ms[j]));
            }
        }
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        vm.resumeGasMetering();
        uint256 g = gasleft();
        bytes32 id = dsp.openRerun(ID, shard);
        emit log_named_uint("openRerun", g - gasleft());
        vm.roll(block.number + 1);
        g = gasleft();
        dsp.draw(id);
        emit log_named_uint("draw, 128 members, none eligible", g - gasleft());
        dsp.arbitrate(id, fields.root);
        uint256 worst;
        for (uint256 k = 0; k < 16; k++) {
            g = gasleft();
            dsp.settleCase(id, 8);
            uint256 used = g - gasleft();
            if (used > worst) worst = used;
        }
        emit log_named_uint(
            "settleCase worst batch (8 members x 32 wrong verdicts + finish)", worst
        );
        assertLt(worst, GAS_CAP, "every batch within budget");
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
        for (uint256 i = 0; i < mem.length; i++) {
            (uint128 sk,,,) = st.validators(mem[i]);
            assertEq(sk, 0, "every wrong verdict slashed, capped at the stake");
        }
        assertEq(st.balanceOf(ms[0]), BOND + 500, "earliest correct commit paid");
    }

    /// Worst case for drawing and closing a panel: 125 eligible members and a 63-member panel,
    /// the largest a valid config can draw (`MAX_PANEL`, M6-L2-F2).
    function test_LargePanelAtTheBound() public {
        _fill(128);
        OnyxRound.RoundConfig memory c = _cfg();
        c.dispute.panelSize = 63;
        c.dispute.minChallengeBond = 10 * 64;
        c.dispute.maxAppeals = 0;
        _openWith(c);
        _toMinersAndVeto();
        bytes32 id = dsp.openRerun(ID, shard);
        vm.roll(block.number + 1);
        uint256 g = gasleft();
        dsp.draw(id);
        uint256 drawGas = g - gasleft();
        emit log_named_uint("draw, 125 eligible, panel of 63", drawGas);
        assertLt(drawGas, GAS_CAP);
        (address[] memory p, uint64 at,,) = dsp.panel(id, 0);
        assertEq(p.length, 63);
        for (uint256 i = 0; i < p.length; i++) {
            bytes32 h = dsp.panelCommitHash(id, 0, p[i], fields.root, keccak256(abi.encode(i)));
            vm.prank(p[i]);
            dsp.commitVote(id, h);
        }
        vm.roll(at + 50);
        for (uint256 i = 0; i < p.length; i++) {
            vm.prank(p[i]);
            dsp.revealVote(id, fields.root, keccak256(abi.encode(i)));
        }
        vm.roll(at + 150);
        g = gasleft();
        dsp.resolve(id);
        emit log_named_uint("resolve, 63 reveals", g - gasleft());
        g = gasleft();
        dsp.settleCase(id, 128);
        uint256 settleGas = g - gasleft();
        emit log_named_uint("settleCase, 128 members, 63 panel fees", settleGas);
        assertLt(settleGas, GAS_CAP);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
    }

    /// M6-L2-F2: no valid config draws a panel above `MAX_PANEL` (63): a first panel of 65 or
    /// 127, or appeals that would grow past it, are refused when the round opens.
    function test_L2_F2_PanelAboveTheCapRefused() public {
        uint32[3] memory sizes = [uint32(127), 65, 17];
        uint32[3] memory appeals = [uint32(0), 0, 2]; // 17 -> 35 -> 71
        for (uint256 k = 0; k < 3; k++) {
            OnyxRound.RoundConfig memory c = _cfg();
            c.dispute.panelSize = sizes[k];
            c.dispute.maxAppeals = appeals[k];
            c.dispute.minChallengeBond = 10 * 128;
            vm.expectRevert(OnyxLedger.BadConfig.selector);
            _openWith(c);
        }
        OnyxRound.RoundConfig memory ok = _cfg();
        ok.dispute.panelSize = 15;
        ok.dispute.maxAppeals = 2; // 15 -> 31 -> 63
        _openWith(ok);
    }

    /// M6-L2-F2: a 63-member panel that reveals 63 different roots (no majority) resolves in
    /// O(1) (tallied per root as votes arrive), and judging it fits the budget.
    function test_L2_F2_SplitPanelAtTheCapWithinBudget() public {
        _fill(128);
        OnyxRound.RoundConfig memory c = _cfg();
        c.dispute.panelSize = 63;
        c.dispute.minChallengeBond = 10 * 64;
        c.dispute.maxAppeals = 0;
        _openWith(c);
        _toMinersAndVeto();
        bytes32 id = dsp.openRerun(ID, shard);
        vm.roll(block.number + 1);
        dsp.draw(id);
        (address[] memory p, uint64 at,,) = dsp.panel(id, 0);
        assertEq(p.length, 63);
        for (uint256 i = 0; i < p.length; i++) {
            bytes32 r = keccak256(abi.encode("junk", i));
            bytes32 h = dsp.panelCommitHash(id, 0, p[i], r, keccak256(abi.encode(i)));
            vm.prank(p[i]);
            dsp.commitVote(id, h);
        }
        vm.roll(at + 50);
        for (uint256 i = 0; i < p.length; i++) {
            vm.prank(p[i]);
            dsp.revealVote(id, keccak256(abi.encode("junk", i)), keccak256(abi.encode(i)));
        }
        vm.roll(at + 150);
        uint256 g = gasleft();
        dsp.resolve(id);
        uint256 resolveGas = g - gasleft();
        emit log_named_uint("resolve, 63 distinct reveals", resolveGas);
        assertLt(resolveGas, 200_000, "O(1) in the panel size");
        dsp.arbitrate(id, fields.root);
        g = gasleft();
        dsp.settleCase(id, 128);
        uint256 settleGas = g - gasleft();
        emit log_named_uint("settleCase, 128 members, 63 panel members slashed", settleGas);
        assertLt(settleGas, GAS_CAP);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
    }

    /// Two miners reveal the honest root; vals[1] disagrees on both: Pending.
    function _toMinersAndVeto() internal {
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mk2, m2, SALT2, fields.root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        _reveal(m2, SALT2, fields.root);
        vm.roll(base + 202);
        st.drawSeed(ID);
        bytes32 seed = st.seedOf(ID);
        address[2] memory mm = [m1, m2];
        for (uint256 j = 0; j < 2; j++) {
            bytes32 h = OnyxRound.verdictCommitHash(
                seed, vals[1], shard, mm[j], OnyxRound.DISAGREE, OTHER, _vsalt(vals[1], mm[j])
            );
            vm.prank(vals[1]);
            st.commitVerdict(ID, shard, mm[j], h);
        }
        vm.roll(base + 300);
        for (uint256 j = 0; j < 2; j++) {
            vm.prank(vals[1]);
            st.revealVerdict(ID, shard, mm[j], OnyxRound.DISAGREE, OTHER, _vsalt(vals[1], mm[j]));
        }
        _openNoCanaries();
        st.finalizeShard(ID, shard);
    }
}
