// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxMarketsFixture} from "./OnyxMarketsFixture.sol";
import {OnyxStakes} from "../src/OnyxStakes.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";
import {OnyxTestnetOwnerOracle} from "../src/OnyxTestnetOwnerOracle.sol";
import {OnyxReobserve} from "../src/OnyxReobserve.sol";
import {IStakeToken, IOnyxReobserve, IOnyxFallbackOracle} from "../src/IOnyxMarkets.sol";

/// ROADMAP S3/S4 (ADR-0042): staked submissions decided by re-observation, then the vote, then
/// the fallback oracle; payouts, duplicates, caps, windows and the attack bounds.
contract OnyxStakesTest is OnyxMarketsFixture {
    address internal v1 = makeAddr("validator1");
    address internal v2 = makeAddr("validator2");
    address internal m1 = makeAddr("miner1");

    function setUp() public override {
        super.setUp();
        _fund(address(this), 100e18);
        stk.fundRewards(10e18);
    }

    // ------------------------------------------------------------------ observation (S4)

    function test_ObservedHonestClaimIsAcceptedAtLevel2AndPaid() public {
        uint256 id = _submit(sub, FACT, VAL, OnyxStakes.Kind.Observed);
        _commit(id, v1, 1, 30e18);
        _commit(id, m1, 2, 10e18);
        _allObserve(id, VAL);
        _reveal(id, v1, 1);
        _reveal(id, m1, 2);
        _toResolve(id);
        stk.resolve(id);
        OnyxStakes.Claim memory c = stk.claim(id);
        assertEq(uint8(c.state), uint8(OnyxStakes.State.Correct));
        assertEq(uint8(c.by), uint8(OnyxStakes.By.Observation));
        (bool fin, int256 v, uint8 level,) = stk.factValue(FACT);
        assertTrue(fin);
        assertEq(v, VAL);
        assertEq(level, 2);
        // Losers 10; fee 2% = 0.2; distributable 9.8 over a winning pool of 40.
        assertEq(_withdraw(id, sub), 10e18 + 10e18 * 98e17 / 40e18 + 1e18);
        assertEq(_withdraw(id, v1), 30e18 + 30e18 * 98e17 / 40e18);
        vm.expectRevert(OnyxStakes.NothingOwed.selector);
        vm.prank(m1);
        stk.withdraw(id);
        assertEq(stk.held(), 0);
        assertEq(stk.treasuryOwed(), 2e17);
        _booksBalance();
        stk.payTreasury();
        assertEq(tok.balanceOf(treasury), 2e17);
    }

    function test_WrongSubmitterIsSlashedAndTheFactReopens() public {
        uint256 id = _submit(sub, FACT, VAL + 45, OnyxStakes.Kind.Observed);
        _commit(id, v2, 2, 20e18);
        _allObserve(id, VAL);
        _reveal(id, v2, 2);
        _toResolve(id);
        stk.resolve(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Wrong));
        vm.expectRevert(OnyxStakes.NothingOwed.selector);
        vm.prank(sub);
        stk.withdraw(id);
        // v2 wins the bond less the 2% fee.
        assertEq(_withdraw(id, v2), 20e18 + 98e17);
        (bool fin,,,) = stk.factValue(FACT);
        assertFalse(fin);
        address honest = makeAddr("honest");
        _fund(honest, 10e18);
        uint256 id2 = _submit(honest, FACT, VAL, OnyxStakes.Kind.Observed);
        assertEq(uint8(stk.claim(id2).state), uint8(OnyxStakes.State.Open));
        _booksBalance();
    }

    function test_ObservationOverridesAWhaleVote() public {
        uint256 id = _submit(sub, FACT, VAL + 1, OnyxStakes.Kind.Observed);
        address[5] memory whales =
            [makeAddr("w1"), makeAddr("w2"), makeAddr("w3"), makeAddr("w4"), makeAddr("w5")];
        for (uint256 i = 0; i < 5; i++) {
            _commit(id, whales[i], 1, 100e18);
        }
        _allObserve(id, VAL);
        for (uint256 i = 0; i < 5; i++) {
            _reveal(id, whales[i], 1);
        }
        _toResolve(id);
        stk.resolve(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Wrong));
        assertEq(uint8(stk.claim(id).by), uint8(OnyxStakes.By.Observation));
        // Nobody backed "wrong": everything lost goes to the treasury.
        assertEq(stk.treasuryOwed(), 510e18);
        _booksBalance();
    }

    function test_NoObservationQuorumFallsToTheVote() public {
        uint256 id = _submit(sub, FACT, VAL, OnyxStakes.Kind.Observed);
        _commit(id, v1, 1, 30e18);
        _observe(id, obs1, 1, VAL);
        _toReveal(id);
        _observeReveal(id, obs1, 1, VAL);
        _reveal(id, v1, 1);
        _toResolve(id);
        (uint8 st,) = ro.outcome(id);
        assertEq(st, ro.NO_RESULT());
        stk.resolve(id);
        assertEq(uint8(stk.claim(id).by), uint8(OnyxStakes.By.Vote));
        (,, uint8 level,) = stk.factValue(FACT);
        assertEq(level, 4);
    }

    // ------------------------------------------------------------------ duplicates

    function test_DuplicateOfAnAcceptedValueEarnsNothing() public {
        uint256 id = _submit(sub, FACT, VAL, OnyxStakes.Kind.Observed);
        _allObserve(id, VAL);
        _toResolve(id);
        stk.resolve(id);
        address dup = makeAddr("dup");
        _fund(dup, 50e18);
        uint256 before = tok.balanceOf(dup);
        uint256 d = _submit(dup, FACT, VAL, OnyxStakes.Kind.Observed);
        assertEq(uint8(stk.claim(d).state), uint8(OnyxStakes.State.Duplicate));
        assertEq(tok.balanceOf(dup), before);
        assertEq(stk.owed(d, dup), 0);
        vm.expectRevert(OnyxStakes.NothingOwed.selector);
        vm.prank(dup);
        stk.withdraw(d);
        vm.expectRevert(OnyxStakes.AlreadyFinal.selector);
        vm.prank(dup);
        stk.submit(FACT, VAL - 1, OnyxStakes.Kind.Observed);
    }

    function test_DuplicateWhileOpenEarnsNothingAndADifferentValueIsRefused() public {
        _submit(sub, FACT, VAL, OnyxStakes.Kind.Observed);
        address dup = makeAddr("dup");
        _fund(dup, 50e18);
        uint256 d = _submit(dup, FACT, VAL, OnyxStakes.Kind.Observed);
        assertEq(uint8(stk.claim(d).state), uint8(OnyxStakes.State.Duplicate));
        assertEq(tok.balanceOf(dup), 50e18);
        vm.expectRevert(OnyxStakes.FactBusy.selector);
        vm.prank(dup);
        stk.submit(FACT, VAL + 1, OnyxStakes.Kind.Observed);
    }

    // ------------------------------------------------------------------ the vote

    function test_VotedClaimDecidedByASupermajorityAtLevel4() public {
        uint256 id = _submit(sub, FACT, 7, OnyxStakes.Kind.Voted);
        _commit(id, v1, 1, 20e18);
        _commit(id, v2, 1, 15e18);
        _commit(id, m1, 2, 15e18);
        _toReveal(id);
        _reveal(id, v1, 1);
        _reveal(id, v2, 1);
        _reveal(id, m1, 2);
        _toResolve(id);
        stk.resolve(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Correct));
        assertEq(uint8(stk.claim(id).by), uint8(OnyxStakes.By.Vote));
        (,, uint8 level,) = stk.factValue(FACT);
        assertEq(level, 4);
    }

    function test_SubmitterBondNeverCountsAsAVote() public {
        // Self-resolution: a lone submitter cannot decide its own claim.
        uint256 id = _submit(sub, FACT, 7, OnyxStakes.Kind.Voted);
        _toResolve(id);
        stk.resolve(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Fallback));
    }

    function test_ContestedClaimGoesToTheFallbackOracle() public {
        uint256 id = _submit(sub, FACT, 7, OnyxStakes.Kind.Voted);
        _commit(id, v1, 1, 15e18);
        _commit(id, m1, 2, 15e18);
        _toReveal(id);
        _reveal(id, v1, 1);
        _reveal(id, m1, 2);
        _toResolve(id);
        stk.resolve(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Fallback));
        vm.expectRevert(OnyxStakes.WrongPhase.selector);
        stk.settleFallback(id);
        orc.answer(id, 2);
        stk.settleFallback(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Wrong));
        assertEq(uint8(stk.claim(id).by), uint8(OnyxStakes.By.Fallback));
        // m1 wins v1's 15 and the bond 10, less 2%.
        assertEq(_withdraw(id, m1), 15e18 + 245e17);
        _booksBalance();
    }

    function test_SilentFallbackVoidsAndRefundsEveryone() public {
        uint256 id = _submit(sub, FACT, 7, OnyxStakes.Kind.Voted);
        _commit(id, v1, 1, 5e18);
        _commit(id, m1, 2, 3e18);
        _toReveal(id);
        _reveal(id, v1, 1);
        // m1 never reveals: refunded too on a void.
        _toResolve(id);
        stk.resolve(id);
        vm.warp(stk.claim(id).fallbackDeadline);
        stk.settleFallback(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Void));
        assertEq(uint8(stk.claim(id).by), uint8(OnyxStakes.By.Timeout));
        assertEq(_withdraw(id, sub), 10e18);
        assertEq(_withdraw(id, v1), 5e18);
        assertEq(_withdraw(id, m1), 3e18);
        assertEq(stk.held(), 0);
        assertEq(stk.treasuryOwed(), 0);
        _booksBalance();
    }

    function test_UnrevealedStakeIsForfeitedToTheTreasury() public {
        uint256 id = _submit(sub, FACT, 7, OnyxStakes.Kind.Voted);
        _commit(id, v1, 1, 25e18);
        _commit(id, m1, 2, 4e18);
        _toReveal(id);
        _reveal(id, v1, 1);
        _toResolve(id);
        stk.resolve(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Correct));
        assertEq(stk.treasuryOwed(), 4e18);
        vm.expectRevert(OnyxStakes.NothingOwed.selector);
        vm.prank(m1);
        stk.withdraw(id);
    }

    // ------------------------------------------------------------------ who, how much, when

    function test_SubmitterArbiterAndObserversMayNotStake() public {
        uint256 id = _submit(sub, FACT, VAL, OnyxStakes.Kind.Observed);
        _fund(owner, 10e18);
        address[3] memory no = [sub, owner, obs1];
        for (uint256 i = 0; i < 3; i++) {
            _fund(no[i], 10e18);
            bytes32 h = stk.commitHash(id, no[i], 1, bytes32(0));
            vm.expectRevert(OnyxStakes.NotAllowed.selector);
            vm.prank(no[i]);
            stk.commitStake(id, h, 5e18);
        }
    }

    function test_StakeCapsAndMinimum() public {
        uint256 id = _submit(sub, FACT, VAL, OnyxStakes.Kind.Voted);
        _fund(v1, 500e18);
        bytes32 h = stk.commitHash(id, v1, 1, bytes32(0));
        vm.startPrank(v1);
        vm.expectRevert(OnyxStakes.BadAmount.selector);
        stk.commitStake(id, h, 1e18 - 1);
        vm.expectRevert(OnyxStakes.BadAmount.selector);
        stk.commitStake(id, h, 100e18 + 1);
        stk.commitStake(id, h, 100e18);
        vm.expectRevert(OnyxStakes.AlreadyCommitted.selector);
        stk.commitStake(id, h, 1e18);
        vm.stopPrank();
        // Fill the per-claim cap (1,000) with nine more addresses at 100.
        for (uint256 i = 0; i < 9; i++) {
            _commit(id, makeAddr(string(abi.encode("filler", i))), 1, 100e18);
        }
        address late = makeAddr("late");
        _fund(late, 10e18);
        bytes32 h2 = stk.commitHash(id, late, 1, bytes32(0));
        vm.expectRevert(OnyxStakes.CapReached.selector);
        vm.prank(late);
        stk.commitStake(id, h2, 1e18);
    }

    function test_CommitCannotBeCopiedByAnotherAddress() public {
        uint256 id = _submit(sub, FACT, 7, OnyxStakes.Kind.Voted);
        _commit(id, v1, 1, 5e18);
        (,,, bytes32 seen) = stk.stakes(id, v1);
        _fund(m1, 5e18);
        vm.prank(m1);
        stk.commitStake(id, seen, 5e18);
        _toReveal(id);
        // The copier knows v1's side and salt only after v1 reveals; even then the hash binds v1.
        _reveal(id, v1, 1);
        vm.expectRevert(OnyxStakes.BadReveal.selector);
        vm.prank(m1);
        stk.revealStake(id, 1, _salt(v1));
    }

    function test_WindowsAreEnforced() public {
        uint256 id = _submit(sub, FACT, 7, OnyxStakes.Kind.Voted);
        _commit(id, v1, 1, 5e18);
        vm.expectRevert(OnyxStakes.WrongPhase.selector);
        _reveal(id, v1, 1);
        vm.expectRevert(OnyxStakes.WrongPhase.selector);
        stk.resolve(id);
        _toReveal(id);
        _fund(m1, 5e18);
        bytes32 h = stk.commitHash(id, m1, 1, bytes32(0));
        vm.expectRevert(OnyxStakes.WrongPhase.selector);
        vm.prank(m1);
        stk.commitStake(id, h, 5e18);
        _toResolve(id);
        vm.expectRevert(OnyxStakes.WrongPhase.selector);
        _reveal(id, v1, 1);
        // Stakes stay locked until the claim is decided.
        vm.expectRevert(OnyxStakes.NothingOwed.selector);
        vm.prank(v1);
        stk.withdraw(id);
    }

    function test_ParamsAreSnapshottedPerClaim() public {
        uint256 id = _submit(sub, FACT, 7, OnyxStakes.Kind.Voted);
        OnyxStakes.Params memory p = _stakeParams();
        p.submitBond = 50e18;
        p.feePpm = 100_000;
        stk.setParams(p);
        assertEq(stk.claim(id).bond, 10e18);
        assertEq(stk.claim(id).paramsId, 0);
        assertEq(stk.currentParams(), 1);
        vm.expectRevert(OnyxStakes.NotOwner.selector);
        vm.prank(v1);
        stk.setParams(p);
    }

    function test_BadParamsAreRefused() public {
        OnyxStakes.Params memory p = _stakeParams();
        p.decisivePpm = 500_000;
        vm.expectRevert(OnyxStakes.BadParams.selector);
        stk.setParams(p);
        p = _stakeParams();
        p.feePpm = 100_001;
        vm.expectRevert(OnyxStakes.BadParams.selector);
        stk.setParams(p);
        p = _stakeParams();
        p.commitWindow = 59;
        vm.expectRevert(OnyxStakes.BadParams.selector);
        stk.setParams(p);
        p = _stakeParams();
        p.maxStakePerAddress = p.maxStakePerClaim + 1;
        vm.expectRevert(OnyxStakes.BadParams.selector);
        stk.setParams(p);
        assertTrue(stk.paramsValid(stk.defaultParams()));
    }

    // ------------------------------------------------------------------ attack bounds (item 9)

    /// Splits `amount` over fresh addresses at the per-address cap and commits them on `side`.
    function _crowd(uint256 id, string memory tag, uint8 side, uint256 amount)
        internal
        returns (address[] memory who)
    {
        uint256 n = (amount + 100e18 - 1) / 100e18;
        who = new address[](n);
        if (n == 0) return who;
        uint256 each = amount / n;
        for (uint256 i = 0; i < n; i++) {
            who[i] = makeAddr(string(abi.encode(tag, i)));
            uint256 a = i + 1 == n ? amount - each * (n - 1) : each;
            _commit(id, who[i], side, uint128(a));
        }
    }

    function _attack(uint256 honest, uint256 attack) internal returns (OnyxStakes.State) {
        uint256 id = _submit(sub, keccak256(abi.encode(honest, attack)), 7, OnyxStakes.Kind.Voted);
        address[] memory h = _crowd(id, "honest", 1, honest);
        address[] memory a = _crowd(id, "attacker", 2, attack);
        _toReveal(id);
        for (uint256 i = 0; i < h.length; i++) {
            _reveal(id, h[i], 1);
        }
        for (uint256 i = 0; i < a.length; i++) {
            _reveal(id, a[i], 2);
        }
        _toResolve(id);
        stk.resolve(id);
        return stk.claim(id).state;
    }

    function test_AttackNeedsTheBoundAndOneTokenLessFails() public {
        uint256 honest = 150e18;
        uint256 need = stk.minAttackStake(0, honest);
        assertEq(need, 300_000_450_000_450_000_451);
        assertEq(uint8(_attack(honest, need)), uint8(OnyxStakes.State.Wrong));
        // One base unit less: not decisive, so the fallback oracle decides instead.
        assertEq(uint8(_attack(honest, need - 1)), uint8(OnyxStakes.State.Fallback));
    }

    function test_AttackIsImpossibleWhenHonestStakeExceedsTheCapShare() public {
        // Above a third of the 1,000 cap, no stake that fits can reach two thirds.
        uint256 honest = 334e18;
        assertEq(stk.minAttackStake(0, honest), 0);
        uint256 id = _submit(sub, FACT, 7, OnyxStakes.Kind.Voted);
        _crowd(id, "honest", 1, honest);
        _crowd(id, "attacker", 2, 600e18);
        _fund(makeAddr("more"), 100e18);
        bytes32 hh = stk.commitHash(id, makeAddr("more"), 2, bytes32(0));
        vm.expectRevert(OnyxStakes.CapReached.selector);
        vm.prank(makeAddr("more"));
        stk.commitStake(id, hh, 67e18);
    }

    function test_LowQuorumIsNeverDecidedByTheVote() public {
        // 19 tokens all on one side, quorum 20: fallback.
        assertEq(uint8(_attack(0, 19e18)), uint8(OnyxStakes.State.Fallback));
        assertEq(stk.minAttackStake(0, 0), 20e18);
    }

    // ------------------------------------------------------------------ guards

    function test_TestTokenAndOwnerOracleRefuseOtherChains() public {
        vm.chainId(1);
        vm.expectRevert(abi.encodeWithSelector(OnyxTestToken.WrongChain.selector, 1));
        new OnyxTestToken(owner);
        vm.expectRevert(abi.encodeWithSelector(OnyxTestToken.WrongChain.selector, 1));
        tok.mint(owner, 1);
        vm.expectRevert(abi.encodeWithSelector(OnyxTestnetOwnerOracle.WrongChain.selector, 1));
        new OnyxTestnetOwnerOracle();
        vm.chainId(84_532);
        new OnyxTestToken(owner);
        vm.chainId(31_337);
        vm.expectRevert(OnyxTestToken.NotMinter.selector);
        vm.prank(v1);
        tok.mint(v1, 1);
    }

    function test_OracleAnswersOnlyAskedClaimsOnceByItsOwner() public {
        vm.expectRevert(OnyxTestnetOwnerOracle.NotAsked.selector);
        orc.answer(1, 1);
        vm.expectRevert(OnyxTestnetOwnerOracle.NotStakes.selector);
        orc.request(1);
        uint256 id = _submit(sub, FACT, 7, OnyxStakes.Kind.Voted);
        _toResolve(id);
        stk.resolve(id);
        vm.expectRevert(OnyxTestnetOwnerOracle.NotOwner.selector);
        vm.prank(v1);
        orc.answer(id, 1);
        vm.expectRevert(OnyxTestnetOwnerOracle.BadAnswer.selector);
        orc.answer(id, 4);
        orc.answer(id, 3);
        vm.expectRevert(OnyxTestnetOwnerOracle.Answered.selector);
        orc.answer(id, 1);
        stk.settleFallback(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Void));
    }

    function test_ReobserveNeedsAMajorityOfAllAssigned() public {
        uint256 id = _submit(sub, FACT, VAL, OnyxStakes.Kind.Observed);
        address stranger = makeAddr("stranger");
        vm.expectRevert(OnyxReobserve.NotAssigned.selector);
        vm.prank(stranger);
        ro.commit(id, bytes32(uint256(1)));
        _observe(id, obs1, 1, VAL);
        _observe(id, obs2, 2, 0);
        _observe(id, obs3, 2, 0);
        _toReveal(id);
        // A reveal that does not match the commit is refused.
        vm.expectRevert(OnyxReobserve.BadReveal.selector);
        vm.prank(obs1);
        ro.reveal(id, 1, VAL + 1, keccak256(abi.encode("obs", obs1)));
        _observeReveal(id, obs1, 1, VAL);
        _observeReveal(id, obs2, 2, 0);
        (uint8 early,) = ro.outcome(id);
        assertEq(early, 0);
        _observeReveal(id, obs3, 2, 0);
        _toResolve(id);
        (uint8 st,) = ro.outcome(id);
        assertEq(st, ro.SOURCE_UNAVAILABLE());
        vm.expectRevert(OnyxReobserve.NotStakes.selector);
        ro.open(99, FACT, 1, 2);
    }

    function test_TooFewObserversGiveNoResult() public {
        ro.removeObserver(obs3);
        uint256 id = _submit(sub, FACT, VAL, OnyxStakes.Kind.Observed);
        _observe(id, obs1, 1, VAL);
        _observe(id, obs2, 1, VAL);
        _toReveal(id);
        _observeReveal(id, obs1, 1, VAL);
        _observeReveal(id, obs2, 1, VAL);
        _toResolve(id);
        (uint8 st,) = ro.outcome(id);
        assertEq(st, ro.NO_RESULT());
    }

    // ------------------------------------------------------------------ conservation (fuzz)

    function testFuzz_EveryTokenIsAccountedFor(
        uint96[6] memory amounts,
        uint8 sides,
        uint8 outcome,
        bool observed
    ) public {
        uint256 id = _submit(
            sub, FACT, VAL, observed ? OnyxStakes.Kind.Observed : OnyxStakes.Kind.Voted
        );
        address[6] memory who;
        uint256 total = 10e18;
        for (uint256 i = 0; i < 6; i++) {
            who[i] = makeAddr(string(abi.encode("fz", i)));
            uint128 a = uint128(bound(amounts[i], 1e18, 100e18));
            total += a;
            _commit(id, who[i], uint8(1 + ((sides >> i) & 1)), a);
        }
        if (observed && outcome % 4 == 0) {
            _allObserve(id, outcome % 8 == 0 ? VAL : VAL + 1);
        } else {
            _toReveal(id);
        }
        for (uint256 i = 0; i < 5; i++) {
            _reveal(id, who[i], uint8(1 + ((sides >> i) & 1)));
        }
        _toResolve(id);
        stk.resolve(id);
        if (stk.claim(id).state == OnyxStakes.State.Fallback) {
            if (outcome % 5 == 4) vm.warp(stk.claim(id).fallbackDeadline);
            else orc.answer(id, 1 + (outcome % 3));
            stk.settleFallback(id);
        }
        uint256 out;
        uint256 treasuryBefore = stk.treasuryOwed();
        uint256 reward = stk.claim(id).reward;
        if (stk.owed(id, sub) > 0) out += _withdraw(id, sub);
        for (uint256 i = 0; i < 6; i++) {
            if (stk.owed(id, who[i]) > 0) out += _withdraw(id, who[i]);
        }
        assertEq(stk.held(), 0);
        assertEq(out + stk.treasuryOwed(), total + reward);
        assertGe(stk.treasuryOwed(), treasuryBefore);
        _booksBalance();
    }
}

/// A token whose transfer re-enters `withdraw`, as a hostile token would.
contract ReenteringToken is IStakeToken {
    mapping(address => uint256) public balanceOf;
    OnyxStakes public target;
    uint256 public claimId;

    function arm(OnyxStakes t, uint256 id) external {
        (target, claimId) = (t, id);
    }

    function mint(address to, uint256 a) external {
        balanceOf[to] += a;
    }

    function transfer(address to, uint256 a) external returns (bool) {
        balanceOf[msg.sender] -= a;
        balanceOf[to] += a;
        if (address(target) != address(0)) target.withdraw(claimId);
        return true;
    }

    function transferFrom(address from, address to, uint256 a) external returns (bool) {
        balanceOf[from] -= a;
        balanceOf[to] += a;
        return true;
    }
}

contract OnyxStakesReentrancyTest is OnyxMarketsFixture {
    function test_WithdrawCannotBeReentered() public {
        ReenteringToken bad = new ReenteringToken();
        OnyxReobserve r = new OnyxReobserve(0);
        OnyxTestnetOwnerOracle o = new OnyxTestnetOwnerOracle();
        OnyxStakes s = new OnyxStakes(
            IStakeToken(address(bad)),
            IOnyxReobserve(address(r)),
            IOnyxFallbackOracle(address(o)),
            treasury,
            _stakeParams()
        );
        r.bind(address(s));
        o.bind(address(s));
        bad.mint(sub, 10e18);
        vm.prank(sub);
        uint256 id = s.submit(FACT, 1, OnyxStakes.Kind.Voted);
        vm.warp(s.claim(id).revealClose);
        s.resolve(id);
        o.answer(id, 3);
        s.settleFallback(id);
        bad.arm(s, id);
        vm.expectRevert(OnyxStakes.Reentrancy.selector);
        vm.prank(sub);
        s.withdraw(id);
    }
}
