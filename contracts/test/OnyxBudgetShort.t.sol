// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Vm} from "forge-std/Test.sol";
import {OnyxDisputesTest} from "./OnyxDisputes.t.sol";
import {OnyxSettlementFixture} from "./OnyxSettlement.t.sol";
import {OnyxCommit} from "../src/OnyxCommit.sol";
import {OnyxDisputes} from "../src/OnyxDisputes.sol";
import {OnyxLedger, IERC20Min} from "../src/OnyxLedger.sol";
import {OnyxRound} from "../src/OnyxRound.sol";
import {OnyxSettlement} from "../src/OnyxSettlement.sol";
import {OnyxSettlementExt} from "../src/OnyxSettlementExt.sol";

/// Review L2-R1 (ADR-0022 Amendment 1): a rerun case pays a validator's fee once per revealed
/// root, but the round reserved one fee per member per shard, and `_spend` used to cap silently
/// at what was left of the round-wide budget. Each shard now draws only on its own reservation,
/// a member's first fee comes from it, further fees come from the case's lost miner bonds, and
/// any shortfall is recorded owed (`SpendShort`) and claimable from the reward pool.
contract OnyxBudgetShortTest is OnyxDisputesTest {
    bytes32 internal constant SHORT_SIG =
        keccak256("SpendShort(uint64,bytes32,address,uint256,uint256)");

    /// Three more bonded voters (six in all), as in the review's PoC.
    function _sixVoters() internal returns (address[6] memory vv) {
        for (uint256 i = 0; i < 3; i++) {
            vv[i] = vals[i];
        }
        for (uint256 i = 0; i < 3; i++) {
            address x = vm.addr(0xE00 + i);
            _fund(x, STAKE);
            vm.prank(x);
            st.stake(STAKE);
            vv[3 + i] = x;
        }
    }

    /// `_solvent` plus the three extra voters and an extra miner.
    function _solventWith(address[6] memory vv, address m3) internal view {
        uint256 sum = st.burned() + st.rewardPool() + st.coordinatorBond() + sx().roundBudget(ID);
        address[] memory w = new address[](9 + spares.length);
        (w[0], w[1], w[2], w[3], w[4], w[5]) = (vv[0], vv[1], vv[2], vv[3], vv[4], vv[5]);
        (w[6], w[7], w[8]) = (m1, m2, m3);
        for (uint256 i = 0; i < spares.length; i++) {
            w[9 + i] = spares[i];
        }
        for (uint256 i = 0; i < w.length; i++) {
            if (w[i] != address(0)) sum += _stake(w[i]) + st.balanceOf(w[i]);
        }
        sum += st.balanceOf(challenger) + st.balanceOf(appellant) + st.balanceOf(address(this));
        assertEq(usdc.balanceOf(address(st)), sum, "escrow == liabilities + burned");
    }

    function _countShort(Vm.Log[] memory logs, uint256 owedEach, uint256 paidEach)
        internal
        view
        returns (uint256 n)
    {
        for (uint256 i = 0; i < logs.length; i++) {
            if (logs[i].emitter != address(st) || logs[i].topics[0] != SHORT_SIG) continue;
            (uint256 owed, uint256 paid) = abi.decode(logs[i].data, (uint256, uint256));
            assertEq(owed, owedEach, "owed per short pay");
            assertEq(paid, paidEach, "paid per short pay");
            n++;
        }
    }

    /// Ported PoC (review L2-R1): two revealed roots, six voters right on both. Before the fix
    /// the 12 fees drained the shard's budget, the miner reward was cut and the honest rerun
    /// panel got 0 instead of 30, silently. Now every rule-mandated payee is paid in full, the
    /// second fee per voter comes from the wrong miner's lost bond, and the budget pays exactly
    /// the shard's mandatory items.
    function test_L2R1_RerunFeesDoNotShortTheMinerOrThePanel() public {
        _spares(3); // the panel pool: no verdicts
        address[6] memory vv = _sixVoters();
        _open();
        uint256 budget0 = sx().roundBudget(ID);
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mk2, m2, SALT2, OTHER);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        _reveal(m2, SALT2, OTHER);
        vm.roll(base + 202);
        st.drawSeed(ID);
        for (uint256 i = 0; i < 6; i++) {
            _commitVR(vv[i], m1, OnyxRound.AGREE, bytes32(0));
            _commitVR(vv[i], m2, OnyxRound.DISAGREE, fields.root);
        }
        vm.roll(base + 300);
        for (uint256 i = 0; i < 6; i++) {
            _revealVR(vv[i], m1, OnyxRound.AGREE, bytes32(0));
            _revealVR(vv[i], m2, OnyxRound.DISAGREE, fields.root);
        }
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Pending));
        bytes32 id = dsp.openRerun(ID, shard);
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        _endAppeals(id, 0);
        dsp.resolve(id);
        uint256 burned0 = st.burned();
        vm.recordLogs();
        dsp.settleCase(id, 100);
        assertEq(_countShort(vm.getRecordedLogs(), 0, 0), 0, "no short pay");
        for (uint256 i = 0; i < 6; i++) {
            assertEq(st.balanceOf(vv[i]), 20, "right on two roots: fee twice (rules)");
        }
        address[] memory panel = _members(id, 0);
        uint256 panelPaid;
        for (uint256 i = 0; i < panel.length; i++) {
            panelPaid += st.balanceOf(panel[i]);
        }
        assertEq(st.balanceOf(m1), BOND + 500, "miner reward paid in full");
        assertEq(panelPaid, 30, "honest rerun panel paid its fees");
        // The budget paid 6 first fees + the reward + the panel; the 6 second fees came from
        // m2's lost bond, of which the rest burned.
        (uint128 spent, uint256 allot) = sx().shardSpent(ID, shard);
        assertEq(spent, 60 + 500 + 30, "shard drew its mandatory items only");
        assertLe(spent, allot, "within the shard's reservation");
        assertEq(sx().roundBudget(ID), budget0 - spent, "budget less exactly what was drawn");
        // Burned: m2's bond less the 6 extra fees, and the 3 spares' missed tasks (every member
        // is assigned at rate 1,000,000 and they answered nothing).
        assertEq(st.burned() - burned0, BOND - 60 + 3 * 100, "lost bond less the extra fees");
        assertEq(sx().owedTotal(), 0, "nothing owed");
        _solventWith(vv, address(0));
    }

    /// Sybil miners revealing distinct wrong roots (the cheap griefing in L2-R1): each wrong
    /// root adds a fee per right voter, paid from that miner's lost bond; the budget and every
    /// honest payee are untouched.
    function test_L2R1_SybilWrongRootsPayTheirOwnExtraFees() public {
        _spares(3);
        address[6] memory vv = _sixVoters();
        uint256 mk3 = 0xB03;
        address m3 = vm.addr(mk3);
        _fund(m3, 10 * BOND);
        _open();
        bytes32 salt3 = keccak256("salt 3");
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mk2, m2, SALT2, OTHER);
        _commit(mk3, m3, salt3, WRONG);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        _reveal(m2, SALT2, OTHER);
        _reveal(m3, salt3, WRONG);
        vm.roll(base + 202);
        st.drawSeed(ID);
        for (uint256 i = 0; i < 6; i++) {
            _commitVR(vv[i], m1, OnyxRound.AGREE, bytes32(0));
            _commitVR(vv[i], m2, OnyxRound.DISAGREE, fields.root);
            _commitVR(vv[i], m3, OnyxRound.DISAGREE, fields.root);
        }
        vm.roll(base + 300);
        for (uint256 i = 0; i < 6; i++) {
            _revealVR(vv[i], m1, OnyxRound.AGREE, bytes32(0));
            _revealVR(vv[i], m2, OnyxRound.DISAGREE, fields.root);
            _revealVR(vv[i], m3, OnyxRound.DISAGREE, fields.root);
        }
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        bytes32 id = dsp.openRerun(ID, shard);
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        _endAppeals(id, 0);
        dsp.resolve(id);
        uint256 burned0 = st.burned();
        // Judged in batches of two members: the lost bonds carry over between calls.
        for (uint256 k = 0; k < 10 && _state(id) == OnyxDisputes.State.Decided; k++) {
            dsp.settleCase(id, 2);
        }
        for (uint256 i = 0; i < 6; i++) {
            assertEq(st.balanceOf(vv[i]), 30, "right on three roots: fee three times");
        }
        assertEq(st.balanceOf(m1), BOND + 500, "miner reward");
        (uint128 spent,) = sx().shardSpent(ID, shard);
        assertEq(spent, 60 + 500 + 30, "extras never touch the budget");
        assertEq(st.burned() - burned0, 2 * BOND - 120 + 3 * 100, "lost bonds less 12 fees");
        assertEq(sx().owedTotal(), 0);
        _solventWith(vv, m3);
    }

    /// A canary's key is a second root with no lost bond behind it (canary bonds return): the
    /// extra fee cannot be paid from the case, so it is recorded owed with `SpendShort`, never
    /// dropped, and `claimOwed` pays it from the reward pool.
    function test_L2R1_UncoveredExtraFeeIsOwedAndClaimable() public {
        _spares(3);
        uint256 mkc = 0xCA1;
        address mc = vm.addr(mkc);
        _fund(mc, 10 * BOND);
        OnyxRound.Canary[] memory list = new OnyxRound.Canary[](1);
        list[0] = OnyxRound.Canary({shard: shard, miner: mc, root: OTHER});
        OnyxRound.RoundConfig memory c = _cfg();
        c.canaryCommit = OnyxRound.canaryCommit(list, CANARY_SALT);
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        st.openRound(ID, c, ids, inits);
        bytes32 saltc = keccak256("canary miner salt");
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mkc, mc, saltc, OTHER);
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        _reveal(mc, saltc, OTHER);
        vm.roll(base + 202);
        st.drawSeed(ID);
        for (uint256 i = 0; i < 3; i++) {
            _commitVR(vals[i], m1, OnyxRound.AGREE, bytes32(0));
            _commitVR(vals[i], mc, OnyxRound.DISAGREE, fields.root);
        }
        vm.roll(base + 300);
        for (uint256 i = 0; i < 3; i++) {
            _revealVR(vals[i], m1, OnyxRound.AGREE, bytes32(0));
            _revealVR(vals[i], mc, OnyxRound.DISAGREE, fields.root);
        }
        vm.roll(base + 400);
        sx().openCanaries(ID, list, CANARY_SALT);
        st.finalizeShard(ID, shard);
        assertEq(uint8(st.shardStatus(ID, shard)), uint8(OnyxLedger.ShardStatus.Pending));
        bytes32 id = dsp.openRerun(ID, shard);
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        _endAppeals(id, 0);
        dsp.resolve(id);
        vm.recordLogs();
        dsp.settleCase(id, 100);
        assertEq(_countShort(vm.getRecordedLogs(), 10, 0), 3, "one SpendShort per uncovered fee");
        for (uint256 i = 0; i < 3; i++) {
            assertEq(st.balanceOf(vals[i]), 10, "first fee paid from the reservation");
            assertEq(sx().owedOf(vals[i]), 10, "the canary-root fee is recorded owed");
        }
        assertEq(sx().owedTotal(), 30);
        assertEq(st.balanceOf(m1), BOND + 500, "miner reward");
        assertEq(st.balanceOf(mc), BOND, "canary bond returns");
        uint256 pool0 = st.rewardPool();
        for (uint256 i = 0; i < 3; i++) {
            sx().claimOwed(vals[i]);
            assertEq(st.balanceOf(vals[i]), 20, "the rules' two fees, in full");
            assertEq(sx().owedOf(vals[i]), 0);
        }
        assertEq(st.rewardPool(), pool0 - 30, "paid from the reward pool");
        assertEq(sx().owedTotal(), 0);
        // Claiming again pays nothing.
        sx().claimOwed(vals[0]);
        assertEq(st.balanceOf(vals[0]), 20);
    }

    function _openWithFee(uint128 fee) internal {
        OnyxRound.RoundConfig memory c = _cfg();
        c.validation.validatorFee = fee;
        c.validation.minCanaryPpm = 200_000; // deterrence: 2,000 x 200,000 >= fee x 1e6
        bytes32[] memory ids = new bytes32[](1);
        ids[0] = shard;
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](1);
        inits[0] = OnyxLedger.ShardInit(
            OnyxCommit.shardFieldsHash(fields), keccak256("params"), keccak256("registry")
        );
        st.openRound(ID, c, ids, inits);
    }

    /// Review L2-R2 PoC (ported): with validatorFee x voters above the wrong miner's bond, a
    /// colluding miner revealing a wrong root let six colluding voters draw extra fees beyond
    /// its lost bond, recorded owed and paid by `claimOwed` from the reward pool (+1,400 net
    /// for the coalition at fee 400, bond 1,000). `openRound` now refuses such a config.
    function test_L2R2_FeeTimesMembersAboveMinBondIsRefused() public {
        _spares(3);
        _sixVoters();
        (address[] memory members,) = st.activeSet();
        uint128 cap = BOND / uint128(members.length);
        vm.expectRevert(OnyxLedger.BadConfig.selector);
        this.openWithFee(400); // the PoC's fee
        vm.expectRevert(OnyxLedger.BadConfig.selector);
        this.openWithFee(cap + 1);
        _openWithFee(cap); // exactly minBond / members opens
        assertEq(sx().roundBudget(ID) > 0, true);
    }

    function openWithFee(uint128 fee) external {
        _openWithFee(fee);
    }

    /// The PoC's attack at the largest fee the rule allows: the wrong miner's lost bond covers
    /// every voter's extra fee, nothing is recorded owed, and `claimOwed` cannot touch the
    /// reward pool. The coalition (wrong miner + six voters) loses on net.
    function test_L2R2_WrongRootCannotFarmTheRewardPool() public {
        _spares(3);
        address[6] memory vv = _sixVoters();
        (address[] memory members,) = st.activeSet();
        uint128 fee = BOND / uint128(members.length);
        _openWithFee(fee);
        _commit(mk1, m1, SALT1, fields.root);
        _commit(mk2, m2, SALT2, OTHER); // the colluding miner's wrong root
        vm.roll(base + 100);
        _reveal(m1, SALT1, fields.root);
        _reveal(m2, SALT2, OTHER);
        vm.roll(base + 202);
        st.drawSeed(ID);
        for (uint256 i = 0; i < 6; i++) {
            _commitVR(vv[i], m1, OnyxRound.AGREE, bytes32(0));
            _commitVR(vv[i], m2, OnyxRound.DISAGREE, fields.root);
        }
        vm.roll(base + 300);
        for (uint256 i = 0; i < 6; i++) {
            _revealVR(vv[i], m1, OnyxRound.AGREE, bytes32(0));
            _revealVR(vv[i], m2, OnyxRound.DISAGREE, fields.root);
        }
        _openNoCanaries();
        st.finalizeShard(ID, shard);
        bytes32 id = dsp.openRerun(ID, shard);
        _draw(id, 0);
        _vote(id, 0, _same(3, fields.root));
        _endAppeals(id, 0);
        dsp.resolve(id);
        uint256 burned0 = st.burned();
        vm.recordLogs();
        dsp.settleCase(id, 100);
        assertEq(_countShort(vm.getRecordedLogs(), 0, 0), 0, "no short pay");
        assertEq(sx().owedTotal(), 0, "nothing owed beyond the lost bond");
        uint256 pool0 = st.rewardPool();
        for (uint256 i = 0; i < 6; i++) {
            sx().claimOwed(vv[i]);
            assertEq(st.balanceOf(vv[i]), 2 * fee, "the rules' two fees");
        }
        assertEq(st.rewardPool(), pool0, "claimOwed drew nothing from the reward pool");
        // m2's bond paid the six extra fees; the rest (and the 3 spares' missed tasks) burned.
        assertEq(st.burned() - burned0, BOND - 6 * fee + 3 * 100, "lost bond less extra fees");
        // Coalition net: six extra fees minus the lost bond.
        assertLe(6 * fee, BOND, "the extras never exceed the colluder's bond");
        _solventWith(vv, address(0));
    }
}

/// The per-shard reservation itself (ADR-0022 Amendment 1), driven by a stand-in dispute game
/// (this test) that asks for more than the rules allow: one shard can never draw another's
/// share of the round budget, and a capped pay is never silent.
contract OnyxShardReservationTest is OnyxSettlementFixture {
    OnyxSettlement internal s2;
    bytes32 internal shardA;
    bytes32 internal shardB;

    /// The binding `openRound` checks (`IDisputesBinding`).
    function settlement() external view returns (address) {
        return address(s2);
    }

    function exposure(address) external pure returns (uint256) {
        return 0;
    }

    function _x() internal view returns (OnyxSettlementExt) {
        return OnyxSettlementExt(address(s2));
    }

    function _two() internal {
        s2 = new OnyxSettlement(
            IERC20Min(address(usdc)),
            address(this),
            address(this),
            "onyx.devnet",
            50,
            1_000,
            1_000,
            500
        );
        usdc.approve(address(s2), type(uint256).max);
        for (uint256 i = 0; i < 3; i++) {
            address v = vm.addr(0xF00 + i);
            usdc.mint(v, STAKE);
            vm.prank(v);
            usdc.approve(address(s2), type(uint256).max);
            vm.prank(v);
            s2.stake(STAKE);
        }
        _x().postCoordinatorBond(2_000);
        s2.fundRewards(100_000);
        bytes32 fh = OnyxCommit.shardFieldsHash(fields);
        bytes32[] memory ids = new bytes32[](2);
        OnyxLedger.ShardInit[] memory inits = new OnyxLedger.ShardInit[](2);
        for (uint256 i = 0; i < 2; i++) {
            bytes32 p = keccak256(abi.encode("params", i));
            ids[i] = OnyxCommit.shardId(fh, p, keccak256("registry"));
            inits[i] = OnyxLedger.ShardInit(fh, p, keccak256("registry"));
        }
        if (ids[0] > ids[1]) {
            (ids[0], ids[1]) = (ids[1], ids[0]);
            (inits[0], inits[1]) = (inits[1], inits[0]);
        }
        (shardA, shardB) = (ids[0], ids[1]);
        s2.openRound(ID, _cfg(), ids, inits);
    }

    function test_L2R1_OneShardCannotDrawAnothersReservation() public {
        _two();
        (, uint256 allot) = _x().shardSpent(ID, shardA);
        // 500 reward + 3 members x 10 + 3 panel x 10.
        assertEq(allot, 560);
        assertEq(_x().roundBudget(ID), 2 * allot, "two shares reserved at open");
        address greedy = address(0x6EED);
        vm.expectEmit(true, true, true, true, address(s2));
        emit OnyxLedger.SpendShort(ID, shardA, greedy, 10_000, allot);
        _x().dSpend(ID, shardA, greedy, 10_000);
        assertEq(s2.balanceOf(greedy), allot, "capped at shard A's share");
        assertEq(_x().owedOf(greedy), 10_000 - allot, "the rest is recorded, never silent");
        // Shard B still has its whole share for its own payees.
        address honest = address(0x0B0B);
        _x().dSpend(ID, shardB, honest, uint128(allot));
        assertEq(s2.balanceOf(honest), allot, "shard B's payees untouched by shard A");
        assertEq(_x().owedOf(honest), 0);
        assertEq(_x().roundBudget(ID), 0);
    }
}
