// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test, console} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {SettlementHandler} from "./SettlementHandler.sol";

/// Money and role invariants of OnyxSettlement + OnyxSettlementExt + OnyxDisputes under random
/// call sequences (L2 contract audit). Run at depth with `FOUNDRY_PROFILE=audit`.
contract SettlementInvariantTest is Test {
    SettlementHandler internal h;

    function setUp() public {
        h = new SettlementHandler();
        targetContract(address(h));
        bytes4[] memory s = new bytes4[](37);
        s[0] = h.stake.selector;
        s[1] = h.requestUnbond.selector;
        s[2] = h.withdrawStake.selector;
        s[3] = h.withdraw.selector;
        s[4] = h.fundRewards.selector;
        s[5] = h.postCoordinatorBond.selector;
        s[6] = h.withdrawCoordinatorBond.selector;
        s[7] = h.openRound.selector;
        s[8] = h.commit.selector;
        s[9] = h.reveal.selector;
        s[10] = h.drawSeed.selector;
        s[11] = h.commitVerdict.selector;
        s[12] = h.revealVerdict.selector;
        s[13] = h.openCanaries.selector;
        s[14] = h.missCanaryDeadline.selector;
        s[15] = h.finalizeShard.selector;
        s[16] = h.voidShard.selector;
        s[17] = h.settleRound.selector;
        s[18] = h.refundVoidedShard.selector;
        s[19] = h.reclaimBudget.selector;
        s[20] = h.openRerun.selector;
        s[21] = h.challenge.selector;
        s[22] = h.revealClaim.selector;
        s[23] = h.draw.selector;
        s[24] = h.commitVote.selector;
        s[25] = h.revealVote.selector;
        s[26] = h.appeal.selector;
        s[27] = h.resolve.selector;
        s[28] = h.arbitrate.selector;
        s[29] = h.expire.selector;
        s[30] = h.settleCase.selector;
        s[31] = h.release.selector;
        s[32] = h.warp.selector;
        s[33] = h.attack.selector;
        s[34] = h.autoRound.selector;
        s[35] = h.autoCase.selector;
        s[36] = h.claimOwed.selector;
        targetSelector(StdInvariant.FuzzSelector({addr: address(h), selectors: s}));
    }

    /// Conservation: every token the settlement holds is a credit, a stake, the reward pool,
    /// the coordinator bond, a round budget, a held miner or dispute bond, or burned. Exact,
    /// so nothing is minted from nothing and nothing leaks.
    function invariant_EscrowEqualsLiabilities() public view {
        assertEq(h.usdc().balanceOf(address(h.st())), h.liabilities(), "escrow != liabilities");
    }

    /// Only deposits bring tokens in and only `withdraw` takes them out, each by what it moved.
    function invariant_FlowsMatchBalance() public view {
        assertEq(h.ghostIn() - h.ghostOut(), h.usdc().balanceOf(address(h.st())), "flows");
    }

    /// Rewards and fees committed to rounds never exceed what was funded.
    function invariant_RewardsNeverExceedFunding() public view {
        assertLe(h.st().rewardPool() + h.sumBudgets(), h.ghostFunded(), "rewards > funded");
    }

    /// A seat on a running panel reserves exactly `panelSlash`; ended cases release it.
    function invariant_ExposureMatchesOpenPanels() public view {
        for (uint256 i = 0; i < h.NV(); i++) {
            address v = h.vals(i);
            assertEq(h.dsp().exposure(v), h.expectedExposure(v), "exposure");
        }
    }

    /// Payouts follow the rules per shard (review L2-R1, ADR-0022 Amendment 1): what left the
    /// round budget for a shard, plus extra fees paid from lost bonds, plus shortfalls recorded
    /// owed, equals what an independent model of the rules owes on it (miner reward once,
    /// verdict fees once per validator and revealed root, a rerun's first-panel fees), and the
    /// recorded shortfalls are exactly the model's uncovered extra fees. A release never pays
    /// less than the miner reward. Catches a payout drained from the budget that conservation
    /// alone cannot see (the reviewer's planted double miner reward).
    function invariant_PayoutsMatchRules() public view {
        assertEq(h.payMismatches(), 0, "a shard's payouts differ from the rules");
        assertEq(h.stat("rewardShort"), 0, "a release paid less than the reward");
        assertEq(
            h.sx().owedTotal() + h.ghostClaimed(),
            h.sumSeenOwed(),
            "owed records != recorded shortfalls"
        );
        assertEq(h.breaches(), 0, h.lastBreach());
    }

    /// Roles are fixed: the coordinator is the handler, the pair is bound both ways, and no
    /// privileged call from another account ever succeeded; final roots never changed; locks
    /// were never escaped; tokens never moved on a path that may not move them.
    function invariant_RolesLocksAndFinality() public view {
        assertEq(h.st().coordinator(), address(h), "coordinator");
        assertEq(h.st().disputes(), address(h.dsp()), "disputes");
        assertEq(address(h.dsp().settlement()), address(h.st()), "binding");
        assertEq(h.breaches(), 0, h.lastBreach());
    }

    /// No stuck funds: after every round, case and stake is driven to its end and everyone
    /// withdraws, the settlement holds exactly what it burned plus the unspent reward pool.
    function afterInvariant() public {
        h.finishAll();
        assertTrue(h.allEnded(), "a shard or case could not end");
        address st = address(h.st());
        assertEq(h.sumCredits(), 0, "credits left");
        assertEq(h.sumStakes(), 0, "stakes left");
        assertEq(h.sumBudgets(), 0, "budgets left");
        assertEq(h.escrow(), 0, "bonds left");
        assertEq(h.st().coordinatorBond(), 0, "coordinator bond left");
        assertEq(h.usdc().balanceOf(st), h.st().burned() + h.st().rewardPool(), "stuck funds");
        assertEq(h.ghostIn() - h.ghostOut(), h.usdc().balanceOf(st), "flows after finish");
        for (uint256 i = 0; i < h.NV(); i++) {
            assertEq(h.dsp().exposure(h.vals(i)), 0, "exposure left");
        }
        assertEq(h.breaches(), 0, h.lastBreach());
        assertEq(h.payMismatches(), 0, "payouts differ from the rules after finish");
        assertTrue(h.sx().owedTotal() == 0 || h.st().rewardPool() == 0, "owed left unclaimed");
        _stats(
            string.concat(
                "STATS settlement rounds=",
                vm.toString(h.stat("openRound")),
                " commits=",
                vm.toString(h.stat("commit")),
                " reveals=",
                vm.toString(h.stat("reveal")),
                " verdicts=",
                vm.toString(h.stat("revealVerdict")),
                " finalized=",
                vm.toString(h.stat("finalized")),
                " pending=",
                vm.toString(h.stat("pending")),
                " voided=",
                vm.toString(h.stat("voidShard") + h.stat("refundVoided"))
            ),
            string.concat(
                " reruns=",
                vm.toString(h.stat("openRerun")),
                " challenges=",
                vm.toString(h.stat("challenge")),
                " draws=",
                vm.toString(h.stat("draw")),
                " votes=",
                vm.toString(h.stat("revealVote")),
                " appeals=",
                vm.toString(h.stat("appeal")),
                " arbitrated=",
                vm.toString(h.stat("arbitrate")),
                " expired=",
                vm.toString(h.stat("expire")),
                " settledCases=",
                vm.toString(h.stat("settleCase")),
                " released=",
                vm.toString(h.stat("release")),
                " withdrawals=",
                vm.toString(h.stat("withdraw")),
                " autoRounds=",
                vm.toString(h.stat("autoRound")),
                " autoCases=",
                vm.toString(h.stat("autoCase")),
                " rewardShort=",
                vm.toString(h.stat("rewardShort")),
                " owedRecorded=",
                vm.toString(h.sumSeenOwed()),
                " claims=",
                vm.toString(h.stat("claimOwed"))
            )
        );
    }

    /// One line per run: printed, and appended to cache/invariant-stats/ when
    /// ONYX_INVARIANT_STATS=1 (the audit run sums them; docs/audit/2026-10-13-contracts.md).
    function _stats(string memory a, string memory b) internal {
        console.log(string.concat(a, b));
        if (vm.envOr("ONYX_INVARIANT_STATS", false)) {
            vm.writeLine("cache/invariant-stats/settlement.log", string.concat(a, b));
        }
    }
}
