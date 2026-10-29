// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {stdStorage, StdStorage} from "forge-std/Test.sol";
import {OnyxSettlementFixture} from "./OnyxSettlement.t.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";
import {OnyxDisputes} from "../src/OnyxDisputes.sol";
import {OnyxLedger, IERC20Min} from "../src/OnyxLedger.sol";
import {OnyxRound} from "../src/OnyxRound.sol";
import {OnyxSettlement} from "../src/OnyxSettlement.sol";
import {OnyxSettlementExt} from "../src/OnyxSettlementExt.sol";
import {OnyxRegistry, IRegistryLedger} from "../src/OnyxRegistry.sol";
import {OnyxMerkle} from "../src/OnyxMerkle.sol";

/// The on-chain dispute game (ADR-0024): rerun cases for Pending shards (M6-F10), challenges
/// of finalized roots, appeals, the coordinator's rerun, expiry, and the money each path moves.
/// Config from `OnyxSettlementFixture._cfg`: panel 3 (then 7), windows 50 blocks, one appeal,
/// panel fee 10, panel slash 500, challenge window 200, min challenge bond 1,000.
contract OnyxDisputesTest is OnyxSettlementFixture {
    using stdStorage for StdStorage;

    bytes32 internal constant OTHER = keccak256("other root");
    bytes32 internal constant WRONG = keccak256("lazy root");
    /// Panel vote placeholder: this member stays silent.
    bytes32 internal constant SILENT = keccak256("silent");
    bytes32 internal constant CLAIM_SALT = keccak256("claim salt");

    address[] internal spares;
    address internal challenger = address(0xC4A1);
    address internal appellant = address(0xA99E);

    // ------------------------------------------------------------------ setup helpers

    /// This suite sets its panel pool per test (`_spares`).
    function _standbys() internal pure override returns (uint256) {
        return 0;
    }

    /// Bonded validators who give no verdict: the panel candidates.
    function _spares(uint256 n) internal {
        for (uint256 i = 0; i < n; i++) {
            address v = vm.addr(0xC00 + i);
            _fund(v, STAKE);
            vm.prank(v);
            st.stake(STAKE);
            spares.push(v);
        }
        _fund(challenger, 100_000);
        _fund(appellant, 100_000);
    }

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

    /// Both miners reveal the honest root; vals[0] and vals[2] agree, vals[1] disagrees with
    /// OTHER on both reveals: the shard goes Pending (the old free veto, M6-F10).
    function _vetoedShard(uint256 nSpares) internal {
        _spares(nSpares);
        _toVerdicts();
        address[2] memory ms = [m1, m2];
        for (uint256 j = 0; j < 2; j++) {
            _commitVR(vals[0], ms[j], OnyxRound.AGREE, bytes32(0));
            _commitVR(vals[1], ms[j], OnyxRound.DISAGREE, OTHER);
            _commitVR(vals[2], ms[j], OnyxRound.AGREE, bytes32(0));
        }
        vm.roll(base + 300);
        for (uint256 j = 0; j < 2; j++) {
            _revealVR(vals[0], ms[j], OnyxRound.AGREE, bytes32(0));
            _revealVR(vals[1], ms[j], OnyxRound.DISAGREE, OTHER);
            _revealVR(vals[2], ms[j], OnyxRound.AGREE, bytes32(0));
        }
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Pending));
    }

    /// Both miners reveal `root` and all three voters agree: the shard finalizes on it.
    function _unanimousOn(bytes32 root, uint256 nSpares) internal {
        _unanimousVotes(root, nSpares);
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
    }

    /// Everything `_unanimousOn` does before settling.
    function _unanimousVotes(bytes32 root, uint256 nSpares) internal {
        _spares(nSpares);
        _open();
        _commit(mk1, m1, SALT1, root);
        _commit(mk2, m2, SALT2, root);
        vm.roll(base + 100);
        _reveal(m1, SALT1, root);
        _reveal(m2, SALT2, root);
        vm.roll(base + 202);
        st.drawSeed(ID);
        _vote(OnyxRound.AGREE, bytes32(0), true);
        _openNoCanaries();
    }

    function _claim(bytes32 root) internal view returns (bytes32) {
        return dsp.claimCommit(st.ledgerId(), ID, shard, challenger, root, CLAIM_SALT);
    }

    function _challenge(bytes32 claimedRoot, uint128 bond) internal returns (bytes32 id) {
        bytes32 claim = _claim(claimedRoot); // before the prank: it makes calls
        vm.prank(challenger);
        id = dsp.challenge(ID, shard, claim, bond);
    }

    /// A challenge expected to revert with `err` (the claim is computed before the cheatcodes).
    function _challengeReverts(bytes4 err, bytes32 claimedRoot, uint128 bond) internal {
        bytes32 claim = _claim(claimedRoot);
        vm.prank(challenger);
        vm.expectRevert(err);
        dsp.challenge(ID, shard, claim, bond);
    }

    function _members(bytes32 id, uint32 lv) internal view returns (address[] memory m) {
        (m,,,) = dsp.panel(id, lv);
    }

    function _psalt(address m) internal pure returns (bytes32) {
        return keccak256(abi.encodePacked("panel salt", m));
    }

    /// Draw the current level one block after it opened.
    function _draw(bytes32 id, uint32 lv) internal {
        (,, uint64 anchor,) = dsp.panel(id, lv);
        vm.roll(anchor + 1);
        dsp.draw(id);
    }

    /// Members commit and reveal `roots[i]` (SILENT: nothing); ends inside the appeal window.
    function _vote(bytes32 id, uint32 lv, bytes32[] memory roots) internal {
        (address[] memory ms, uint64 at,,) = dsp.panel(id, lv);
        assertEq(ms.length, roots.length, "panel size");
        for (uint256 i = 0; i < ms.length; i++) {
            if (roots[i] == SILENT) continue;
            bytes32 h = dsp.panelCommitHash(id, lv, ms[i], roots[i], _psalt(ms[i]));
            vm.prank(ms[i]);
            dsp.commitVote(id, h);
        }
        vm.roll(at + 50);
        for (uint256 i = 0; i < ms.length; i++) {
            if (roots[i] == SILENT) continue;
            vm.prank(ms[i]);
            dsp.revealVote(id, roots[i], _psalt(ms[i]));
        }
        vm.roll(at + 100); // appeal window open
    }

    function _endAppeals(bytes32 id, uint32 lv) internal {
        (, uint64 at,,) = dsp.panel(id, lv);
        vm.roll(at + 150);
    }

    function _three(bytes32 a, bytes32 b, bytes32 c) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](3);
        (r[0], r[1], r[2]) = (a, b, c);
    }

    function _same(uint256 n, bytes32 a) internal pure returns (bytes32[] memory r) {
        r = new bytes32[](n);
        for (uint256 i = 0; i < n; i++) {
            r[i] = a;
        }
    }

    function _stake(address v) internal view returns (uint128 s) {
        (s,,,) = st.validators(v);
    }

    function _state(bytes32 id) internal view returns (OnyxDisputes.State s) {
        (,,, s,,,) = dsp.caseInfo(id);
    }

    /// Every token the settlement holds is owed to someone or burned.
    function _solvent() internal view {
        uint256 sum = st.burned() + st.rewardPool() + st.coordinatorBond() + sx().roundBudget(ID);
        address[] memory w = new address[](7 + spares.length);
        (w[0], w[1], w[2], w[3], w[4], w[5], w[6]) =
        (vals[0], vals[1], vals[2], m1, m2, challenger, appellant);
        for (uint256 i = 0; i < spares.length; i++) {
            w[7 + i] = spares[i];
        }
        for (uint256 i = 0; i < w.length; i++) {
            sum += _stake(w[i]) + st.balanceOf(w[i]);
        }
        sum += st.balanceOf(address(this));
        assertEq(usdc.balanceOf(address(st)), sum, "escrow == liabilities + burned");
    }

    function _noExposure(bytes32 id, uint32 levels) internal view {
        for (uint32 lv = 0; lv < levels; lv++) {
            address[] memory ms = _members(id, lv);
            for (uint256 i = 0; i < ms.length; i++) {
                assertEq(dsp.exposure(ms[i]), 0, "exposure released");
            }
        }
    }

    // ------------------------------------------------------------------ rerun cases (M6-F10)

    /// REG-R1-F7: a shard decided by a rerun case keeps the level attested before its open, and
    /// the registry's leveled row check holds once the case makes it final.
    function test_RegistryLevelHoldsAfterACaseDecidesTheShard() public {
        OnyxRegistry reg = new OnyxRegistry(address(0x60F), IRegistryLedger(address(st)));
        bytes32 metric = keccak256("crypto.btc.entity_flow");
        vm.startPrank(address(0x60F));
        reg.registerMetric(metric, OnyxRegistry.Kind.Recipe, fields.recipeId, 0);
        reg.registerLabelSet(fields.labelSetHash, 4);
        vm.stopPrank();
        OnyxRegistry.ShardPreimage memory p = OnyxRegistry.ShardPreimage({
            recipeId: fields.recipeId,
            runtimeHash: fields.runtimeHash,
            labelSetHash: fields.labelSetHash,
            sourceHash: fields.sourceHash,
            schemaId: fields.schemaId,
            rightsHash: fields.rightsHash,
            paramsHash: keccak256("params"),
            registryRoot: keccak256("registry")
        });
        assertEq(reg.attestShard(ID, p, runtimeBytes, metric, 1_000, 4), shard);
        _vetoedShard(4);
        bytes32[] memory sib = new bytes32[](1);
        sib[0] = OnyxMerkle.leafHash(rows[1]);
        (bool ok, uint8 level,) = reg.verifyRowLeveled(ID, shard, rows[0], 0, 2, sib);
        assertFalse(ok, "Pending: not final, no level");
        bytes32 id = dsp.openRerun(ID, shard);
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        _endAppeals(id, 0);
        dsp.resolve(id);
        dsp.settleCase(id, 100);
        assertTrue(sx().isFinal(ID, shard));
        bytes32 m;
        (ok, level, m) = reg.verifyRowLeveled(ID, shard, rows[0], 0, 2, sib);
        assertTrue(ok);
        assertEq(level, 4);
        assertEq(m, metric);
    }

    /// The old free veto now costs the vetoer: a panel attests the honest root, the shard
    /// finalizes, both wrong Disagree verdicts are slashed and the honest side is paid.
    function test_RerunCaseClosesTheFreeVeto() public {
        _vetoedShard(4);
        bytes32 id = dsp.openRerun(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.InCase));
        _draw(id, 0);
        address[] memory panel = _members(id, 0);
        assertEq(panel.length, 3);
        for (uint256 i = 0; i < 3; i++) {
            assertEq(dsp.exposure(panel[i]), 500, "a drawn member stakes panelSlash");
        }
        _vote(id, 0, _same(3, fields.root));
        _endAppeals(id, 0);
        dsp.resolve(id);
        assertEq(uint8(_state(id)), uint8(OnyxDisputes.State.Decided));
        dsp.settleCase(id, 2); // batched: 2 members per call
        dsp.settleCase(id, 2);
        dsp.settleCase(id, 100);
        assertEq(uint8(_state(id)), uint8(OnyxDisputes.State.Done));

        assertTrue(sx().isFinal(ID, shard), "decided by a case: final at once");
        (bytes32 root, bytes32 binding,) = sx().getFinalizedRoot(ID, shard);
        assertEq(root, fields.root);
        assertEq(binding, OnyxCommit.bindingHash(fields));
        assertEq(_stake(vals[1]), STAKE - 2 * 2_000, "each wrong Disagree slashed");
        assertEq(st.balanceOf(vals[0]), 10, "fee once per (validator, shard, root)");
        assertEq(st.balanceOf(vals[2]), 10);
        assertEq(st.balanceOf(m1), BOND + 500, "earliest correct commit paid");
        assertEq(st.balanceOf(m2), BOND, "bond back");
        for (uint256 i = 0; i < 3; i++) {
            assertEq(st.balanceOf(panel[i]), 10, "panel fee from the round budget");
        }
        _noExposure(id, 1);
        for (uint256 i = 0; i < spares.length; i++) {
            assertEq(_stake(spares[i]), STAKE - 100, "assigned and silent: missed task");
        }
        assertTrue(st.everFinalized(shard));
        sx().reclaimBudget(ID);
        _solvent();
    }

    /// The panel finds no valid root: every reveal is wrong, so both miner bonds are lost,
    /// Agree verdicts are wrong and Disagree with a root is wrong too (Rust L25-F1).
    function test_RerunCaseNoValidRoot() public {
        _vetoedShard(4);
        bytes32 id = dsp.openRerun(ID, shard);
        _draw(id, 0);
        _vote(id, 0, _same(3, dsp.noValidRootMarker(shard)));
        _endAppeals(id, 0);
        dsp.resolve(id);
        dsp.settleCase(id, 100);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.NoValidRoot));
        (bytes32 root,,) = sx().getFinalizedRoot(ID, shard);
        assertEq(root, bytes32(0));
        assertEq(st.balanceOf(m1), 0, "bond lost");
        assertEq(st.balanceOf(m2), 0, "bond lost");
        for (uint256 i = 0; i < 3; i++) {
            assertEq(_stake(vals[i]), STAKE - 2 * 2_000, "two wrong verdicts each");
        }
        assertTrue(st.everFinalized(shard));
        sx().reclaimBudget(ID);
        _solvent();
    }

    /// Nobody draws the panel in its 256-block beacon window: the case goes to the rerun
    /// instead of reverting (M5-L49-F3).
    function test_DrawLapseGoesToRerun() public {
        _vetoedShard(4);
        bytes32 id = dsp.openRerun(ID, shard);
        vm.roll(block.number + 257);
        dsp.draw(id);
        assertEq(uint8(_state(id)), uint8(OnyxDisputes.State.RerunRequired));
        dsp.arbitrate(id, fields.root); // the coordinator (this test) posts its rerun
        dsp.settleCase(id, 100);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
        assertEq(_stake(vals[1]), STAKE - 4_000);
        sx().reclaimBudget(ID);
        _solvent();
    }

    /// No majority and no appeal, and the coordinator never reruns: Unresolved. Revealed bonds
    /// return, nobody is paid or slashed, the shard is never final (Rust `expire_rerun`).
    function test_SplitPanelExpiresUnresolved() public {
        _vetoedShard(4);
        bytes32 id = dsp.openRerun(ID, shard);
        _draw(id, 0);
        address[] memory panel = _members(id, 0);
        _vote(id, 0, _three(fields.root, OTHER, SILENT));
        _endAppeals(id, 0);
        dsp.resolve(id);
        assertEq(uint8(_state(id)), uint8(OnyxDisputes.State.RerunRequired));
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.expire(id); // the rerun is still due
        (,,,,, uint64 deadline,) = dsp.caseInfo(id);
        vm.roll(deadline);
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.arbitrate(id, fields.root); // too late
        dsp.expire(id);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Unresolved));
        assertFalse(sx().isFinal(ID, shard));
        assertEq(st.balanceOf(m1), BOND, "bond back, no reward");
        assertEq(st.balanceOf(m2), BOND);
        assertEq(_stake(vals[1]), STAKE, "nobody judged");
        for (uint256 i = 0; i < 3; i++) {
            assertEq(st.balanceOf(panel[i]), 0);
        }
        _noExposure(id, 1);
        sx().reclaimBudget(ID);
        _solvent();
    }

    // ------------------------------------------------------------------ challenges

    /// A frivolous challenge: the panel confirms the root. The challenger pays its panel and
    /// loses the rest, half to the earliest correct miner (UMA-style split).
    function test_ChallengeUpheld() public {
        _unanimousOn(fields.root, 4);
        bytes32 id = _challenge(OTHER, 1_000);
        assertEq(usdc.balanceOf(challenger), 100_000 - 1_000, "bond pulled into escrow");
        (bytes32 served,,) = sx().getFinalizedRoot(ID, shard);
        assertEq(served, bytes32(0), "not served while in a case");
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        _endAppeals(id, 0);
        dsp.resolve(id);
        dsp.settleCase(id, 100);
        assertTrue(sx().isFinal(ID, shard));
        assertEq(st.balanceOf(m1), BOND + 500 + 485, "bond, reward and half the rest");
        assertEq(st.balanceOf(m2), BOND);
        assertEq(st.balanceOf(challenger), 0);
        for (uint256 i = 0; i < 3; i++) {
            assertEq(st.balanceOf(vals[i]), 10, "verdict fees untouched");
            assertEq(_stake(vals[i]), STAKE);
        }
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.release(ID, shard); // the case paid the miners
        _challengeReverts(OnyxLedger.WrongPhase.selector, OTHER, 1_000); // closed: no second challenge
        sx().reclaimBudget(ID);
        _solvent();
    }

    /// Lazy miners agree on a wrong root and lazy validators agree with them. The challenger
    /// (claim committed, revealed after the panel) overturns it: the miners lose their held
    /// bonds, half to the challenger, and every Agree is slashed.
    function test_ChallengeOverturned() public {
        _unanimousOn(WRONG, 4);
        bytes32 id = _challenge(fields.root, 1_000);
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        vm.prank(challenger);
        dsp.revealClaim(id, fields.root, CLAIM_SALT);
        _endAppeals(id, 0);
        dsp.resolve(id);
        dsp.settleCase(id, 100);
        (bytes32 root, bytes32 binding,) = sx().getFinalizedRoot(ID, shard);
        assertEq(root, fields.root, "corrected on chain");
        assertEq(binding, bytes32(0), "no miner revealed it: rows not published");
        assertEq(st.balanceOf(m1), 0, "lazy bond lost");
        assertEq(st.balanceOf(m2), 0);
        assertEq(st.balanceOf(challenger), 970 + 1_000, "rest of bond + half the miner bonds");
        for (uint256 i = 0; i < 3; i++) {
            assertEq(_stake(vals[i]), STAKE - 2 * 2_000, "each wrong Agree slashed");
            assertEq(st.balanceOf(vals[i]), 10, "the fee is kept; the slash exceeds it");
        }
        sx().reclaimBudget(ID);
        _solvent();
    }

    /// The root was wrong, but so was the challenger's claim: Corrected, challenger loses.
    function test_ChallengeCorrected() public {
        _unanimousOn(WRONG, 4);
        bytes32 id = _challenge(OTHER, 1_000);
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        vm.prank(challenger);
        dsp.revealClaim(id, OTHER, CLAIM_SALT);
        _endAppeals(id, 0);
        dsp.resolve(id);
        dsp.settleCase(id, 100);
        (bytes32 root,,) = sx().getFinalizedRoot(ID, shard);
        assertEq(root, fields.root);
        assertEq(st.balanceOf(challenger), 0, "claim was not the decided root");
        sx().reclaimBudget(ID);
        _solvent();
    }

    /// A copier cannot reveal someone else's claim, and a wrong reveal is refused.
    function test_ClaimRevealIsBoundToTheChallenger() public {
        _unanimousOn(WRONG, 4);
        bytes32 id = _challenge(fields.root, 1_000);
        vm.prank(appellant);
        vm.expectRevert(OnyxDisputes.BadClaim.selector);
        dsp.revealClaim(id, fields.root, CLAIM_SALT);
        vm.prank(challenger);
        vm.expectRevert(OnyxDisputes.BadClaim.selector);
        dsp.revealClaim(id, OTHER, CLAIM_SALT);
    }

    /// A bribed 2-of-3 majority is appealed to a 7-member panel, which upholds the root. The
    /// bribed members are slashed, the appellant keeps its bond minus its panel's fees.
    function test_AppealToLargerPanelOverrulesBribedMajority() public {
        _unanimousOn(fields.root, 10);
        bytes32 id = _challenge(OTHER, 1_000);
        _draw(id, 0);
        address[] memory p0 = _members(id, 0);
        _vote(id, 0, _three(OTHER, OTHER, fields.root));
        vm.prank(appellant);
        dsp.appeal(id, fields.root, 2_000);
        _draw(id, 1);
        address[] memory p1 = _members(id, 1);
        assertEq(p1.length, 7, "2k + 1");
        for (uint256 i = 0; i < 7; i++) {
            for (uint256 j = 0; j < 3; j++) {
                assertTrue(p1[i] != p0[j], "earlier panels are never drawn again");
            }
        }
        _vote(id, 1, _same(7, fields.root));
        _endAppeals(id, 1);
        dsp.resolve(id);
        dsp.settleCase(id, 100);
        assertTrue(sx().isFinal(ID, shard));
        assertEq(_stake(p0[0]), STAKE - 100 - 500, "bribed: panel slash (and missed task)");
        assertEq(_stake(p0[1]), STAKE - 100 - 500);
        assertEq(st.balanceOf(p0[2]), 10, "honest member paid by the challenger");
        assertEq(st.balanceOf(appellant), 2_000 - 70, "backed the root: keeps the rest");
        for (uint256 i = 0; i < 7; i++) {
            assertEq(st.balanceOf(p1[i]), 10);
        }
        assertEq(st.balanceOf(m1), BOND + 500 + 495, "half of the challenger's rest");
        _noExposure(id, 2);
        sx().reclaimBudget(ID);
        _solvent();
    }

    /// M5-L49-F1 and F3: an appeal when no larger panel exists is taken (no revert) and sends
    /// the case to the coordinator's rerun. The appellant backing the wrong root loses its
    /// bond, and the silent member of a panel where someone found the root is slashed.
    function test_AppealWithNoLargerPanelIsNotFree() public {
        _unanimousOn(fields.root, 4);
        bytes32 id = _challenge(OTHER, 1_000);
        _draw(id, 0);
        address[] memory p0 = _members(id, 0);
        _vote(id, 0, _three(fields.root, fields.root, SILENT));
        vm.prank(appellant);
        dsp.appeal(id, OTHER, 2_000); // only 1 candidate left for a 7-member panel
        _draw(id, 1);
        assertEq(uint8(_state(id)), uint8(OnyxDisputes.State.RerunRequired), "no revert");
        vm.prank(appellant);
        vm.expectRevert(OnyxDisputes.NotCoordinator.selector);
        dsp.arbitrate(id, OTHER);
        dsp.arbitrate(id, fields.root);
        dsp.settleCase(id, 100);
        assertEq(st.balanceOf(appellant), 0, "backed another root: bond lost");
        assertEq(_stake(p0[2]), STAKE - 100 - 500, "silent while a peer found the root");
        assertEq(st.balanceOf(p0[0]), 10);
        assertEq(st.balanceOf(p0[1]), 10);
        assertEq(st.balanceOf(m1), BOND + 500 + 490, "upheld: half of 980");
        sx().reclaimBudget(ID);
        _solvent();
    }

    /// M5-L49-F3: a challenge whose panel is never drawn is accepted and goes to the rerun.
    /// (Too few candidates at the first draw needs the pool to shrink after settlement: the
    /// pool rule keeps `panelSize + 1` at finalize, M6-L2-F1; the draw's own shortfall path
    /// is `test_AppealWithNoLargerPanelIsNotFree`.)
    function test_ChallengeWithNoPanelGoesToRerun() public {
        _unanimousOn(fields.root, 4);
        bytes32 id = _challenge(OTHER, 1_000);
        (,, uint64 anchor,) = dsp.panel(id, 0);
        vm.roll(anchor + dsp.DRAW_WINDOW() + 1);
        dsp.draw(id);
        assertEq(uint8(_state(id)), uint8(OnyxDisputes.State.RerunRequired));
        dsp.arbitrate(id, fields.root);
        dsp.settleCase(id, 100);
        assertTrue(sx().isFinal(ID, shard));
        assertEq(st.balanceOf(m1), BOND + 500 + 500, "no panel fees: half of 1,000");
        sx().reclaimBudget(ID);
        _solvent();
    }

    // ------------------------------------------------------------------ review M6-L2

    /// M6-L2-F1 (Rust L29-F4): with every validator a verdict-giver no panel could ever be
    /// drawn, so three blind Agrees must not finalize a root; the shard goes to a rerun case.
    /// The pool must exceed `panelSize` (3) by one, for a challenger from the same pool.
    function test_L2_F1_UndisputableUnanimousShardIsRerun() public {
        _unanimousVotes(WRONG, 0);
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Pending));
        dsp.openRerun(ID, shard); // the rerun case decides it
        vm.roll(base + 400 + 200);
        (bytes32 served,,) = sx().getFinalizedRoot(ID, shard);
        assertEq(served, bytes32(0), "never served");
    }

    function test_L2_F1_PoolOfPanelSizeIsNotEnough() public {
        _unanimousVotes(fields.root, 3);
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Pending));
    }

    function test_L2_F1_PoolOfPanelSizePlusOneFinalizes() public {
        _unanimousVotes(fields.root, 4);
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Finalized));
    }

    /// A spare whose stake less open exposure cannot cover `panelSlash` does not count.
    function test_L2_F1_ExposedSpareDoesNotCount() public {
        _unanimousVotes(fields.root, 4);
        // Exposure the dispute contract holds for spares[0] (an open panel elsewhere): its
        // stake less that and its missed task (100) falls 1 short of `panelSlash`.
        stdstore.target(address(dsp))
            .sig("exposure(address)")
            .with_key(spares[0])
            .checked_write(STAKE - 100 - 499);
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Pending));
    }

    /// M6-L2-F3: the appeal floor is set by the config (`minChallengeBond * 2^level`), not by
    /// the bond the challenger chose. A rich challenger who bribes 2 of 3 cannot price an
    /// honest appellant out; the larger panel overturns the bribed majority.
    function test_L2_F3_RichChallengerCannotPriceOutTheAppeal() public {
        _unanimousOn(fields.root, 10);
        _fund(challenger, 10_000_000);
        bytes32 id = _challenge(OTHER, 10_000_000);
        _draw(id, 0);
        _vote(id, 0, _three(OTHER, OTHER, fields.root));
        vm.prank(appellant);
        vm.expectRevert(OnyxDisputes.BondTooLow.selector);
        dsp.appeal(id, fields.root, 1_999); // below 1,000 x 2^1
        vm.prank(appellant);
        dsp.appeal(id, fields.root, 2_000);
        _draw(id, 1);
        _vote(id, 1, _same(7, fields.root));
        _endAppeals(id, 1);
        dsp.resolve(id);
        dsp.settleCase(id, 100);
        assertTrue(sx().isFinal(ID, shard));
        assertEq(st.balanceOf(appellant), 2_000 - 70, "backed the root: keeps the rest");
        assertEq(st.balanceOf(challenger), 0, "the bribe failed: bond lost");
        sx().reclaimBudget(ID);
        _solvent();
    }

    /// M6-L2-F5: `expire` charges only panels that sat. The appeal's panel was never drawn
    /// (too few candidates), so the appellant gets its whole bond back; the challenger pays
    /// its own panel's fees.
    function test_L2_F5_ExpireChargesOnlyPanelsThatSat() public {
        _unanimousOn(fields.root, 4);
        bytes32 id = _challenge(OTHER, 1_000);
        _draw(id, 0);
        _vote(id, 0, _three(fields.root, fields.root, SILENT));
        vm.prank(appellant);
        dsp.appeal(id, OTHER, 2_000);
        _draw(id, 1);
        assertEq(_members(id, 1).length, 0, "no panel");
        (,,,,, uint64 deadline,) = dsp.caseInfo(id);
        vm.roll(deadline);
        dsp.expire(id);
        assertEq(st.balanceOf(appellant), 2_000, "its panel never sat: nothing to pay for");
        assertEq(st.balanceOf(challenger), 1_000 - 30, "its panel sat: 3 fees");
        sx().reclaimBudget(ID);
        _solvent();
    }

    /// M6-L2-F4: a rerun deadline never passes `lockEnd - SETTLE_MARGIN`, so the decided case
    /// is judged while stakes are locked. A case sent to the rerun later can only expire.
    function test_L2_F4_RerunDeadlineLeavesTheSettleMargin() public {
        _unanimousOn(fields.root, 4);
        bytes32 id = _challenge(OTHER, 1_000);
        (,, uint64 anchor,) = dsp.panel(id, 0);
        // pendingClose (base + 600) + tail 1276 = lockEnd base + 1876; the cap is 64 earlier.
        uint256 cap = base + 1876 - 64;
        vm.roll(cap - 10);
        assertGt(block.number, uint256(anchor) + dsp.DRAW_WINDOW());
        dsp.resolve(id); // never drawn: to the rerun
        (,,,,, uint64 deadline,) = dsp.caseInfo(id);
        assertEq(deadline, cap, "capped, not cap - 10 + challengeWindow");
        vm.roll(cap);
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.arbitrate(id, fields.root);
        dsp.expire(id);
    }

    /// Offer `shard` again in round `id2`, configured from the current block; `err` (if not
    /// zero) is the expected revert.
    function _offerAgain(uint64 id2, bytes4 err) internal {
        uint64 saved = base;
        base = uint64(block.number);
        OnyxRound.RoundConfig memory c = _cfg();
        base = saved;
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit({
            fieldsHash: OnyxCommit.shardFieldsHash(fields),
            paramsHash: keccak256("params"),
            registryRoot: keccak256("registry")
        });
        if (err != bytes4(0)) vm.expectRevert(err);
        st.openRound(id2, c, ids, inits);
    }

    /// M6-L3-F1: a shard whose rerun case is running cannot be offered in another round, so
    /// it is never finalized and paid twice; once the case closes it, it is never offered again.
    function test_L3_F1_LiveShardCannotBeOfferedAgain() public {
        _vetoedShard(4);
        assertTrue(st.live(shard));
        _offerAgain(2, OnyxLedger.ShardLive.selector);
        bytes32 id = dsp.openRerun(ID, shard);
        _offerAgain(2, OnyxLedger.ShardLive.selector);
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        _endAppeals(id, 0);
        dsp.resolve(id);
        dsp.settleCase(id, 100);
        assertEq(st.balanceOf(m1), BOND + 500, "paid once");
        _offerAgain(2, OnyxLedger.AlreadyFinalized.selector);
    }

    /// M6-L3-F1: work that ended with no public reveal (Empty) may be offered again (Rust
    /// L23-F4).
    function test_L3_F1_EmptyWorkIsOfferedAgain() public {
        _open();
        vm.roll(base + 202);
        st.drawSeed(ID);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Empty));
        assertFalse(st.live(shard));
        _offerAgain(2, bytes4(0));
        assertTrue(st.live(shard));
    }

    /// M6-L3-F1: work voided after its reveals became public is never offered again (Rust
    /// L26-F2: burned work).
    function test_L3_F1_VoidedRevealedWorkStaysTaken() public {
        _toVerdicts();
        _vote(OnyxRound.DISAGREE, keccak256("another root"), true);
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        vm.roll(base + 600);
        st.voidShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Voided));
        assertTrue(st.live(shard), "reveals are public: burned work");
        _offerAgain(2, OnyxLedger.ShardLive.selector);
    }

    /// M6-L3-F2: a `resolve` front-run does not cost a right challenger its claim. Judging
    /// waits `CLAIM_GRACE` blocks for an unrevealed claim, which can still be revealed.
    function test_L3_F2_ClaimCanBeRevealedAfterAFrontRunResolve() public {
        _unanimousOn(WRONG, 4);
        bytes32 id = _challenge(fields.root, 1_000);
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        _endAppeals(id, 0);
        dsp.resolve(id); // first block after the window
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.settleCase(id, 100);
        vm.prank(challenger);
        dsp.revealClaim(id, fields.root, CLAIM_SALT);
        dsp.settleCase(id, 100); // revealed: no need to wait
        assertEq(st.balanceOf(challenger), 970 + 1_000, "Overturned: rest + half the miner bonds");
        _solvent();
    }

    /// M6-L3-F2: an unrevealed claim waits `CLAIM_GRACE` blocks only; then the case is judged
    /// and the challenger, whose claim was never shown, loses its bond (Corrected).
    function test_L3_F2_UnrevealedClaimWaitsOnlyTheGrace() public {
        _unanimousOn(WRONG, 4);
        bytes32 id = _challenge(fields.root, 1_000);
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        _endAppeals(id, 0);
        dsp.resolve(id);
        vm.roll(block.number + dsp.CLAIM_GRACE());
        dsp.settleCase(id, 100);
        vm.prank(challenger);
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.revealClaim(id, fields.root, CLAIM_SALT);
        assertEq(st.balanceOf(challenger), 0);
        _solvent();
    }

    /// M6-L3-F3: a drawn panel where nobody revealed costs its funder the full panel's fees,
    /// burned, as Rust L30-F1 (`an_all_silent_panel_slashes_nobody_and_costs_the_challenger_
    /// its_fees`): here the challenger's silent level-0 panel, then an appeal panel upholds.
    function test_L3_F3_SilentPanelCostsItsFunderTheFees() public {
        _unanimousOn(fields.root, 11);
        bytes32 id = _challenge(OTHER, 1_000);
        _draw(id, 0);
        _vote(id, 0, _same(3, SILENT));
        vm.prank(appellant);
        dsp.appeal(id, fields.root, 2_000);
        _draw(id, 1);
        _vote(id, 1, _same(7, fields.root));
        _endAppeals(id, 1);
        uint256 burnedBefore = st.burned();
        address[] memory silent = _members(id, 0);
        uint128[] memory stakes = new uint128[](silent.length);
        for (uint256 i = 0; i < silent.length; i++) {
            stakes[i] = _stake(silent[i]);
        }
        dsp.resolve(id);
        dsp.settleCase(id, 100);
        for (uint256 i = 0; i < silent.length; i++) {
            assertEq(_stake(silent[i]), stakes[i], "nobody on a silent panel is slashed");
        }
        // Rest 1,000 - 30 burned = 970: half to the first correct miner, half burned.
        assertEq(st.balanceOf(m1), BOND + 500 + 485);
        assertEq(st.balanceOf(appellant), 2_000 - 70, "backed R: rest of its bond");
        assertEq(st.burned() - burnedBefore, 30 + 485, "silent fees + the burned half");
        _solvent();
    }

    // ------------------------------------------------------------------ rules and access

    function test_CaseEntryRules() public {
        _vetoedShard(4);
        _challengeReverts(OnyxLedger.WrongPhase.selector, OTHER, 1_000); // Pending: rerun case only
        vm.roll(base + 600);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        dsp.openRerun(ID, shard); // settling has closed
    }

    function test_ChallengeEntryRules() public {
        _unanimousOn(fields.root, 4);
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.openRerun(ID, shard); // Finalized: challenge only
        _challengeReverts(OnyxDisputes.BondTooLow.selector, OTHER, 999);
        vm.roll(base + 400 + 200);
        _challengeReverts(OnyxLedger.WrongPhase.selector, OTHER, 1_000); // window over
        dsp.release(ID, shard);
        assertEq(st.balanceOf(m1), BOND + 500);
    }

    function test_InCaseShardCannotBeVoidedOrReleased() public {
        _vetoedShard(4);
        dsp.openRerun(ID, shard);
        vm.roll(base + 600);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        st.voidShard(ID, shard);
        vm.expectRevert(OnyxLedger.WrongPhase.selector);
        sx().reclaimBudget(ID);
    }

    function test_PanelVoteRules() public {
        _vetoedShard(4);
        bytes32 id = dsp.openRerun(ID, shard);
        _draw(id, 0);
        address[] memory p = _members(id, 0);
        vm.prank(vals[0]);
        vm.expectRevert(OnyxDisputes.NotMember.selector);
        dsp.commitVote(id, keccak256("x"));
        bytes32 h = dsp.panelCommitHash(id, 0, p[0], fields.root, _psalt(p[0]));
        vm.prank(p[0]);
        dsp.commitVote(id, h);
        vm.prank(p[0]);
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.revealVote(id, fields.root, _psalt(p[0])); // commits still open
        (, uint64 at,,) = dsp.panel(id, 0);
        vm.roll(at + 50);
        vm.prank(p[0]);
        vm.expectRevert(OnyxDisputes.BadVote.selector);
        dsp.revealVote(id, OTHER, _psalt(p[0]));
        vm.prank(p[0]);
        dsp.revealVote(id, fields.root, _psalt(p[0]));
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.resolve(id); // appeal window still open
        vm.roll(at + 100);
        vm.prank(appellant);
        vm.expectRevert(OnyxDisputes.BondTooLow.selector);
        dsp.appeal(id, fields.root, 1_999);
    }

    function test_UnanimousPanelCannotBeAppealed() public {
        _vetoedShard(4);
        bytes32 id = dsp.openRerun(ID, shard);
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        vm.prank(appellant);
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.appeal(id, OTHER, 2_000);
    }

    /// Only the bound dispute contract moves money, and only through the settlement.
    function test_HooksAreGuarded() public {
        vm.expectRevert(OnyxLedger.NotDisputes.selector);
        sx().dCredit(address(this), 1);
        OnyxSettlementExt direct = OnyxSettlementExt(st.ext());
        vm.prank(address(dsp));
        vm.expectRevert(OnyxLedger.NotDisputes.selector);
        direct.dCredit(address(this), 1); // the extension's own storage: refused
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        dsp.bind(sx()); // bound once
    }

    function test_OpenRoundNeedsBoundDisputes() public {
        OnyxDisputes loose = new OnyxDisputes();
        OnyxSettlement s2 = new OnyxSettlement(
            IERC20Min(address(usdc)), address(this), address(loose), "onyx.devnet", 50, 1, 1, 1
        );
        bytes32[] memory ids = new bytes32[](0);
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](0);
        OnyxRound.RoundConfig memory c = _cfg();
        vm.expectRevert(OnyxLedger.NotDisputes.selector);
        s2.openRound(ID, c, ids, inits);
        vm.prank(address(0xbad));
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        loose.bind(OnyxSettlementExt(address(s2))); // only the deployer binds
        vm.expectRevert(OnyxDisputes.WrongPhase.selector);
        loose.bind(sx()); // st names another dispute contract
    }
}
