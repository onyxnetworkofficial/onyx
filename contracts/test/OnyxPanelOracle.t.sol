// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";
import {OnyxStakes} from "../src/OnyxStakes.sol";
import {OnyxReobserve} from "../src/OnyxReobserve.sol";
import {OnyxPanelOracle} from "../src/OnyxPanelOracle.sol";
import {
    IStakeToken,
    IOnyxReobserve,
    IOnyxFallbackOracle,
    IOnyxValidatorSet
} from "../src/IOnyxMarkets.sol";

/// ROADMAP S3b (ADR-0042 amendment 1): the drawn-panel fallback oracle wired to the real stake
/// contract. A contested claim is answered by a panel of bonded seats that never staked on it.
contract OnyxPanelOracleTest is Test {
    OnyxTestToken internal tok;
    OnyxReobserve internal ro;
    OnyxPanelOracle internal pan;
    OnyxStakes internal stk;

    address internal treasury = address(0x7EA5);
    address internal sub = makeAddr("submitter");
    address internal v1 = makeAddr("validator1");
    address internal m1 = makeAddr("miner1");
    address[] internal seat;

    uint64 internal constant W = 100;
    uint128 internal constant SEAT = 50e18;
    IOnyxValidatorSet internal constant NOGATE = IOnyxValidatorSet(address(0));
    bytes32 internal constant FACT = keccak256("fact one");

    function setUp() public {
        vm.warp(1_790_000_000);
        vm.roll(1_000);
        tok = new OnyxTestToken(address(this));
        ro = new OnyxReobserve(0);
        pan = new OnyxPanelOracle(
            IStakeToken(address(tok)), treasury, SEAT, 5, 3, 100_000, W, W, NOGATE
        );
        OnyxStakes.Params memory p = OnyxStakes.Params({
            submitBond: 10e18,
            submitReward: 0,
            minStake: 1e18,
            maxStakePerAddress: 100e18,
            maxStakePerClaim: 1_000e18,
            quorum: 20e18,
            decisivePpm: 666_667,
            feePpm: 20_000,
            commitWindow: W,
            revealWindow: W,
            // The panel needs a draw plus two windows: give it room.
            fallbackWindow: 4 * W
        });
        stk = new OnyxStakes(
            IStakeToken(address(tok)),
            IOnyxReobserve(address(ro)),
            IOnyxFallbackOracle(address(pan)),
            treasury,
            p
        );
        ro.bind(address(stk));
        pan.bind(address(stk));
        for (uint256 i = 0; i < 8; i++) {
            address s = makeAddr(string.concat("seat", vm.toString(i)));
            seat.push(s);
            _fund(s, SEAT);
            vm.prank(s);
            pan.join();
        }
        _fund(sub, 100e18);
        // Seats are eligible only for claims anchored in a later block.
        vm.roll(block.number + 1);
    }

    function _fund(address who, uint256 amount) internal {
        tok.mint(who, amount);
        vm.startPrank(who);
        tok.approve(address(stk), type(uint256).max);
        tok.approve(address(pan), type(uint256).max);
        vm.stopPrank();
    }

    function _salt(address who) internal pure returns (bytes32) {
        return keccak256(abi.encode("salt", who));
    }

    function _stake(uint256 id, address who, uint8 side, uint128 amount) internal {
        if (tok.balanceOf(who) < amount) _fund(who, amount);
        bytes32 h = stk.commitHash(id, who, side, _salt(who));
        vm.prank(who);
        stk.commitStake(id, h, amount);
    }

    /// A contested claim (15 v 15) that has fallen back to the panel.
    function _contested() internal returns (uint256 id) {
        vm.prank(sub);
        id = stk.submit(FACT, 7, OnyxStakes.Kind.Voted);
        _stake(id, v1, 1, 15e18);
        _stake(id, m1, 2, 15e18);
        vm.warp(stk.claim(id).commitClose);
        vm.prank(v1);
        stk.revealStake(id, 1, _salt(v1));
        vm.prank(m1);
        stk.revealStake(id, 2, _salt(m1));
        vm.warp(stk.claim(id).revealClose);
        stk.resolve(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Fallback));
    }

    function _drawn(uint256 id) internal returns (address[] memory members) {
        vm.roll(block.number + 1);
        pan.draw(id);
        (members,,,) = pan.panel(id);
    }

    function _vote(uint256 id, address m, uint8 a) internal {
        // Hash first: a prank applies to the next call, and `voteHash` is one.
        bytes32 h = pan.voteHash(id, m, a, _salt(m));
        vm.prank(m);
        pan.commitVote(id, h);
    }

    function _reveal(uint256 id, address m, uint8 a) internal {
        vm.prank(m);
        pan.revealVote(id, a, _salt(m));
    }

    function _toReveal(uint256 id) internal {
        (,, uint64 cc,) = pan.panel(id);
        vm.warp(cc);
    }

    function _toFinal(uint256 id) internal {
        (,,, uint64 rc) = pan.panel(id);
        vm.warp(rc);
    }

    function test_PanelMajorityDecidesAContestedClaim() public {
        uint256 id = _contested();
        address[] memory p = _drawn(id);
        assertEq(p.length, 5);
        for (uint256 i = 0; i < 5; i++) {
            _vote(id, p[i], i < 3 ? 2 : 1);
        }
        _toReveal(id);
        for (uint256 i = 0; i < 5; i++) {
            _reveal(id, p[i], i < 3 ? 2 : 1);
        }
        vm.expectRevert(OnyxPanelOracle.WrongPhase.selector);
        pan.finalize(id);
        _toFinal(id);
        pan.finalize(id);
        assertEq(pan.answerOf(id), 2);
        stk.settleFallback(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Wrong));
        assertEq(uint8(stk.claim(id).by), uint8(OnyxStakes.By.Fallback));
        (,, uint8 level,) = stk.factValue(FACT);
        assertEq(level, 0, "a wrong claim leaves the fact open");
        // The minority is not slashed; everyone is released and may leave.
        for (uint256 i = 0; i < 5; i++) {
            assertEq(pan.busy(p[i]), 0);
            vm.prank(p[i]);
            pan.leave();
            assertEq(pan.refundable(p[i]), SEAT);
        }
    }

    function test_NoMajorityIsVoidAndRefundsTheStakers() public {
        uint256 id = _contested();
        address[] memory p = _drawn(id);
        uint8[5] memory a = [uint8(1), 1, 2, 2, 3];
        for (uint256 i = 0; i < 5; i++) {
            _vote(id, p[i], a[i]);
        }
        _toReveal(id);
        for (uint256 i = 0; i < 5; i++) {
            _reveal(id, p[i], a[i]);
        }
        _toFinal(id);
        pan.finalize(id);
        assertEq(pan.answerOf(id), 3);
        stk.settleFallback(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Void));
    }

    function test_StakersAndTheSubmitterAreNeverDrawn() public {
        // Three of the eight seats stake on the claim (and the submitter holds a seat): the panel
        // is drawn from the other five only.
        _fund(sub, SEAT);
        vm.prank(sub);
        pan.join();
        vm.roll(block.number + 1);
        vm.prank(sub);
        uint256 id = stk.submit(FACT, 7, OnyxStakes.Kind.Voted);
        _stake(id, seat[0], 1, 10e18);
        _stake(id, seat[1], 2, 10e18);
        _stake(id, seat[2], 2, 1e18);
        vm.warp(stk.claim(id).revealClose);
        stk.resolve(id);
        address[] memory p = _drawn(id);
        assertEq(p.length, 5);
        for (uint256 i = 0; i < 5; i++) {
            assertTrue(p[i] != sub && p[i] != seat[0] && p[i] != seat[1] && p[i] != seat[2]);
        }
        // The panel contract itself is the arbiter: it can never stake.
        assertEq(pan.arbiter(), address(pan));
        assertFalse(stk.mayStake(id, address(pan)));
    }

    function test_TooFewEligibleSeatsAnswerVoid() public {
        vm.prank(sub);
        uint256 id = stk.submit(FACT, 7, OnyxStakes.Kind.Voted);
        for (uint256 i = 0; i < 4; i++) {
            _stake(id, seat[i], 1, 1e18);
        }
        vm.warp(stk.claim(id).revealClose);
        stk.resolve(id);
        _drawn(id);
        assertEq(pan.answerOf(id), 3);
        stk.settleFallback(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Void));
    }

    function test_NoShowsAreSlashedAndLoseTheirSeat() public {
        uint256 id = _contested();
        address[] memory p = _drawn(id);
        // Three agree; two never reveal (one never commits).
        for (uint256 i = 0; i < 4; i++) {
            _vote(id, p[i], 1);
        }
        _toReveal(id);
        for (uint256 i = 0; i < 3; i++) {
            _reveal(id, p[i], 1);
        }
        _toFinal(id);
        pan.finalize(id);
        assertEq(pan.answerOf(id), 1);
        uint256 cut = uint256(SEAT) / 10;
        for (uint256 i = 3; i < 5; i++) {
            assertFalse(pan.isSeated(p[i]));
            assertEq(pan.refundable(p[i]), SEAT - cut);
            uint256 b = tok.balanceOf(p[i]);
            vm.prank(p[i]);
            pan.withdraw();
            assertEq(tok.balanceOf(p[i]) - b, SEAT - cut);
        }
        assertEq(pan.treasuryOwed(), 2 * cut);
        pan.payTreasury();
        assertEq(tok.balanceOf(treasury), 2 * cut);
        assertEq(pan.seats().length, 6);
        // Tokens conserved: seats still bonded plus nothing owed.
        assertEq(tok.balanceOf(address(pan)), 6 * uint256(SEAT));
    }

    function test_ADrawnSeatCannotLeaveUntilFinalized() public {
        uint256 id = _contested();
        address[] memory p = _drawn(id);
        vm.expectRevert(OnyxPanelOracle.Busy.selector);
        vm.prank(p[0]);
        pan.leave();
        _vote(id, p[0], 1);
        _toReveal(id);
        _reveal(id, p[0], 1);
        _toFinal(id);
        pan.finalize(id);
        vm.prank(p[0]);
        pan.leave();
        // A no-show lost its seat at the finalize, so it has nothing left to leave.
        vm.expectRevert(OnyxPanelOracle.NoSeat.selector);
        vm.prank(p[1]);
        pan.leave();
    }

    function test_VotesAreBoundToMemberAnswerAndWindow() public {
        uint256 id = _contested();
        address[] memory p = _drawn(id);
        address outsider = makeAddr("outsider");
        vm.expectRevert(OnyxPanelOracle.NotDrawn.selector);
        vm.prank(outsider);
        pan.commitVote(id, bytes32(uint256(1)));
        _vote(id, p[0], 1);
        vm.expectRevert(OnyxPanelOracle.AlreadyVoted.selector);
        vm.prank(p[0]);
        pan.commitVote(id, bytes32(uint256(2)));
        // Too early to reveal.
        vm.expectRevert(OnyxPanelOracle.WrongPhase.selector);
        _reveal(id, p[0], 1);
        _toReveal(id);
        // Too late to commit; a changed answer does not open the commit.
        bytes32 late = pan.voteHash(id, p[1], 1, _salt(p[1]));
        vm.expectRevert(OnyxPanelOracle.WrongPhase.selector);
        vm.prank(p[1]);
        pan.commitVote(id, late);
        vm.expectRevert(OnyxPanelOracle.BadReveal.selector);
        _reveal(id, p[0], 2);
        vm.expectRevert(OnyxPanelOracle.BadAnswer.selector);
        _reveal(id, p[0], 4);
        _reveal(id, p[0], 1);
        vm.expectRevert(OnyxPanelOracle.BadReveal.selector);
        _reveal(id, p[0], 1);
    }

    function test_OnlyTheStakeContractAsksAndOnlyOnce() public {
        vm.expectRevert(OnyxPanelOracle.NotStakes.selector);
        pan.request(1);
        uint256 id = _contested();
        vm.expectRevert(OnyxPanelOracle.WrongPhase.selector);
        vm.prank(address(stk));
        pan.request(id);
        vm.expectRevert(OnyxPanelOracle.NotAsked.selector);
        pan.draw(id + 1);
        // Not in the request's own block: its hash is not known yet.
        vm.expectRevert(OnyxPanelOracle.WrongPhase.selector);
        pan.draw(id);
        _drawn(id);
        vm.expectRevert(OnyxPanelOracle.WrongPhase.selector);
        pan.draw(id);
        vm.expectRevert(OnyxPanelOracle.AlreadyBound.selector);
        pan.bind(address(1));
    }

    function test_ALapsedBeaconIsVoidNeverReRolled() public {
        uint256 id = _contested();
        (, uint64 anchor,,) = pan.panel(id);
        vm.roll(uint256(anchor) + 257);
        pan.draw(id);
        (address[] memory none,,,) = pan.panel(id);
        assertEq(none.length, 0);
        assertEq(pan.answerOf(id), 3);
        vm.expectRevert(OnyxPanelOracle.WrongPhase.selector);
        pan.draw(id);
        stk.settleFallback(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Void));
    }

    function test_SeatsJoinedAtOrAfterTheAnchorAreNotEligible() public {
        // Grinding: once the beacon is known, an attacker could pick addresses with small keys
        // and join them. Only seats from before the request's block are drawn.
        vm.prank(sub);
        uint256 id = stk.submit(FACT, 7, OnyxStakes.Kind.Voted);
        for (uint256 i = 0; i < 4; i++) {
            _stake(id, seat[i], 1, 1e18);
        }
        vm.warp(stk.claim(id).revealClose);
        stk.resolve(id);
        // Four eligible seats remain; two late seats in the anchor block and one after it.
        address[3] memory late = [makeAddr("late0"), makeAddr("late1"), makeAddr("late2")];
        for (uint256 i = 0; i < 3; i++) {
            if (i == 2) vm.roll(block.number + 1);
            _fund(late[i], SEAT);
            vm.prank(late[i]);
            pan.join();
        }
        vm.roll(block.number + 1);
        pan.draw(id);
        assertEq(pan.answerOf(id), 3, "late seats do not fill the panel");
    }

    function test_TheDrawIsTheSmallestKeysInOrder() public {
        uint256 id = _contested();
        address[] memory p = _drawn(id);
        (, uint64 anchor,,) = pan.panel(id);
        bytes32 beacon = blockhash(anchor);
        // Every drawn key is below every undrawn key, and keys are in order.
        bytes32 maxDrawn;
        for (uint256 i = 0; i < p.length; i++) {
            bytes32 k = pan.panelKey(id, beacon, p[i]);
            if (i > 0) assertTrue(k > pan.panelKey(id, beacon, p[i - 1]));
            if (k > maxDrawn) maxDrawn = k;
        }
        address[] memory all = pan.seats();
        for (uint256 i = 0; i < all.length; i++) {
            bool drawn;
            for (uint256 j = 0; j < p.length; j++) {
                drawn = drawn || all[i] == p[j];
            }
            if (!drawn) assertTrue(pan.panelKey(id, beacon, all[i]) > maxDrawn);
        }
    }

    function test_SilentPanelTimesOutToVoid() public {
        uint256 id = _contested();
        _drawn(id);
        vm.warp(stk.claim(id).fallbackDeadline);
        stk.settleFallback(id);
        assertEq(uint8(stk.claim(id).state), uint8(OnyxStakes.State.Void));
        assertEq(uint8(stk.claim(id).by), uint8(OnyxStakes.By.Timeout));
    }

    function test_BadParamsAndWrongChainAreRefused() public {
        IStakeToken t = IStakeToken(address(tok));
        vm.expectRevert(OnyxPanelOracle.BadParams.selector);
        new OnyxPanelOracle(t, treasury, SEAT, 4, 2, 0, W, W, NOGATE); // 2 of 4 is no majority
        vm.expectRevert(OnyxPanelOracle.BadParams.selector);
        new OnyxPanelOracle(t, treasury, SEAT, 5, 6, 0, W, W, NOGATE);
        vm.expectRevert(OnyxPanelOracle.BadParams.selector);
        new OnyxPanelOracle(t, treasury, 0, 5, 3, 0, W, W, NOGATE);
        vm.expectRevert(OnyxPanelOracle.BadParams.selector);
        new OnyxPanelOracle(t, treasury, SEAT, 5, 3, 1_000_001, W, W, NOGATE);
        vm.chainId(8453);
        vm.expectRevert(abi.encodeWithSelector(OnyxPanelOracle.WrongChain.selector, 8453));
        new OnyxPanelOracle(t, treasury, SEAT, 5, 3, 0, W, W, NOGATE);
    }

    function test_SeatsAreOnePerAddressAndCapped() public {
        vm.expectRevert(OnyxPanelOracle.Seated.selector);
        vm.prank(seat[0]);
        pan.join();
        vm.expectRevert(OnyxPanelOracle.NoSeat.selector);
        vm.prank(sub);
        pan.leave();
        for (uint256 i = pan.seats().length; i < pan.MAX_SEATS(); i++) {
            address s = address(uint160(0x10000 + i));
            _fund(s, SEAT);
            vm.prank(s);
            pan.join();
        }
        address late = makeAddr("late");
        _fund(late, SEAT);
        vm.expectRevert(OnyxPanelOracle.Full.selector);
        vm.prank(late);
        pan.join();
    }

    /// Any mix of answers and no-shows: the answer is the one with at least `majority` reveals,
    /// else void; every no-show loses exactly the cut; the contract's books balance.
    function testFuzz_TallyIsTheMajorityOrVoid(uint8[5] memory raw) public {
        uint256 id = _contested();
        address[] memory p = _drawn(id);
        uint256[4] memory n;
        for (uint256 i = 0; i < 5; i++) {
            uint8 a = raw[i] % 4; // 0: no-show
            raw[i] = a;
            n[a] += 1;
            if (a != 0) _vote(id, p[i], a);
        }
        _toReveal(id);
        for (uint256 i = 0; i < 5; i++) {
            if (raw[i] != 0) _reveal(id, p[i], raw[i]);
        }
        _toFinal(id);
        pan.finalize(id);
        uint8 want = 3;
        for (uint8 a = 1; a <= 3; a++) {
            if (n[a] >= 3) want = a;
        }
        assertEq(pan.answerOf(id), want);
        uint256 cut = uint256(SEAT) / 10;
        assertEq(pan.treasuryOwed(), n[0] * cut);
        assertEq(pan.seats().length, 8 - n[0]);
        uint256 refunds;
        for (uint256 i = 0; i < 5; i++) {
            refunds += pan.refundable(p[i]);
        }
        assertEq(
            tok.balanceOf(address(pan)),
            pan.seats().length * uint256(SEAT) + refunds + pan.treasuryOwed()
        );
    }
}

/// A validator set the test sets (`OnyxSettlement.validators` shape).
contract SetValidators is IOnyxValidatorSet {
    mapping(address => bool) public on;
    mapping(address => uint64) public unbonding;

    function set(address who, bool active_) external {
        on[who] = active_;
    }

    function unbond(address who) external {
        unbonding[who] = uint64(block.timestamp) + 1 days;
    }

    function validators(address who) external view returns (uint128, uint64, uint64, bool) {
        return (on[who] ? 100e6 : 0, 0, unbonding[who], on[who]);
    }
}

/// THREATS LC16-2: with the settlement's staked validators as the gate, one funded actor cannot
/// fill the panel with fresh addresses, and a seat whose validator is leaving is never drawn.
contract OnyxPanelGateTest is Test {
    OnyxTestToken internal tok;
    OnyxPanelOracle internal pan;
    OnyxStakes internal stk;
    SetValidators internal vs;
    address internal sub = makeAddr("submitter");
    address internal v1 = makeAddr("staker1");
    address internal m1 = makeAddr("staker2");
    address[] internal seat;
    uint64 internal constant W = 100;
    uint128 internal constant SEAT = 50e18;

    function setUp() public {
        vm.warp(1_790_000_000);
        vm.roll(1_000);
        tok = new OnyxTestToken(address(this));
        vs = new SetValidators();
        OnyxReobserve ro = new OnyxReobserve(0);
        pan = new OnyxPanelOracle(
            IStakeToken(address(tok)), address(0x7EA5), SEAT, 5, 3, 100_000, W, W, vs
        );
        OnyxStakes.Params memory p = OnyxStakes.Params({
            submitBond: 10e18,
            submitReward: 0,
            minStake: 1e18,
            maxStakePerAddress: 100e18,
            maxStakePerClaim: 1_000e18,
            quorum: 20e18,
            decisivePpm: 666_667,
            feePpm: 20_000,
            commitWindow: W,
            revealWindow: W,
            fallbackWindow: 4 * W
        });
        stk = new OnyxStakes(
            IStakeToken(address(tok)),
            IOnyxReobserve(address(ro)),
            IOnyxFallbackOracle(address(pan)),
            address(0x7EA5),
            p
        );
        ro.bind(address(stk));
        pan.bind(address(stk));
        for (uint256 i = 0; i < 6; i++) {
            address s = makeAddr(string.concat("validator", vm.toString(i)));
            vs.set(s, true);
            seat.push(s);
            _fund(s, SEAT);
            vm.prank(s);
            pan.join();
        }
        vm.roll(block.number + 1);
    }

    function _fund(address who, uint256 amount) internal {
        tok.mint(who, amount);
        vm.startPrank(who);
        tok.approve(address(stk), type(uint256).max);
        tok.approve(address(pan), type(uint256).max);
        vm.stopPrank();
    }

    function test_OneOperatorCannotFillThePanel() public {
        for (uint256 i = 0; i < 20; i++) {
            address a = makeAddr(string.concat("sybil", vm.toString(i)));
            _fund(a, SEAT);
            vm.prank(a);
            vm.expectRevert(OnyxPanelOracle.NotValidator.selector);
            pan.join();
        }
        assertEq(pan.seats().length, 6, "only staked validators hold seats");
        // A validator that starts unbonding is never drawn.
        vs.unbond(seat[0]);
        assertFalse(pan.isValidator(seat[0]));
        _fund(sub, 100e18);
        vm.prank(sub);
        uint256 id = stk.submit(keccak256("fact"), 7, OnyxStakes.Kind.Voted);
        bytes32 s1 = keccak256("s1");
        bytes32 s2 = keccak256("s2");
        _fund(v1, 15e18);
        _fund(m1, 15e18);
        bytes32 h1 = stk.commitHash(id, v1, 1, s1);
        bytes32 h2 = stk.commitHash(id, m1, 2, s2);
        vm.prank(v1);
        stk.commitStake(id, h1, 15e18);
        vm.prank(m1);
        stk.commitStake(id, h2, 15e18);
        vm.warp(stk.claim(id).commitClose);
        vm.prank(v1);
        stk.revealStake(id, 1, s1);
        vm.prank(m1);
        stk.revealStake(id, 2, s2);
        vm.warp(stk.claim(id).revealClose);
        stk.resolve(id);
        vm.roll(block.number + 1);
        pan.draw(id);
        (address[] memory members,,,) = pan.panel(id);
        assertEq(members.length, 5, "the five staying validators");
        for (uint256 i = 0; i < members.length; i++) {
            assertTrue(members[i] != seat[0], "the leaving validator is not drawn");
        }
    }
}
