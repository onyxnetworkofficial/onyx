// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxMarketsFixture} from "./OnyxMarketsFixture.sol";
import {OnyxStakes} from "../src/OnyxStakes.sol";
import {OnyxFutures} from "../src/OnyxFutures.sol";

/// ROADMAP S6 (ADR-0042 items 11-14): parimutuel markets on a future value of an Onyx fact,
/// resolved only from the fact's re-observed answer.
contract OnyxFuturesTest is OnyxMarketsFixture {
    address internal dave = makeAddr("dave");
    address internal erin = makeAddr("erin");
    address internal frank = makeAddr("frank");

    uint64 internal close;
    uint64 internal observable;
    uint64 internal deadline;

    function setUp() public override {
        super.setUp();
        close = uint64(block.timestamp) + 1 days;
        observable = close + 1 hours;
        deadline = observable + 7 days;
    }

    function _edges() internal pure returns (int256[] memory e) {
        e = new int256[](2);
        e[0] = 4_030_000_000_000_000;
        e[1] = 4_031_000_000_000_000;
    }

    function _open() internal returns (uint256) {
        return fut.createMarket(FACT, _edges(), close, observable, deadline);
    }

    function _bet(uint256 id, address who, uint8 b, uint128 a) internal {
        if (tok.balanceOf(who) < a) _fund(who, a);
        vm.prank(who);
        fut.bet(id, b, a);
    }

    function _acceptObserved(int256 v) internal {
        uint256 id = _submit(sub, FACT, v, OnyxStakes.Kind.Observed);
        _allObserve(id, v);
        _toResolve(id);
        stk.resolve(id);
    }

    function _fwithdraw(uint256 id, address who) internal returns (uint256 got) {
        uint256 b = tok.balanceOf(who);
        vm.prank(who);
        fut.withdraw(id);
        got = tok.balanceOf(who) - b;
    }

    function test_MarketTradesClosesResolvesFromTheReobservedFactAndPays() public {
        uint256 id = _open();
        _bet(id, dave, 0, 30e18);
        _bet(id, erin, 1, 50e18);
        _bet(id, frank, 2, 20e18);
        vm.warp(close);
        _fund(erin, 1e18);
        vm.expectRevert(OnyxFutures.WrongPhase.selector);
        vm.prank(erin);
        fut.bet(id, 1, 1e18);
        // No answer yet: too early to resolve.
        vm.expectRevert(OnyxFutures.WrongPhase.selector);
        fut.resolve(id);
        vm.warp(observable);
        _acceptObserved(VAL);
        fut.resolve(id);
        OnyxFutures.Market memory m = fut.market(id);
        assertEq(uint8(m.state), uint8(OnyxFutures.State.Resolved));
        assertEq(m.winner, 1);
        // Losers 50; fee 1; erin gets 50 + 49.
        assertEq(_fwithdraw(id, erin), 99e18);
        vm.expectRevert(OnyxFutures.NothingOwed.selector);
        vm.prank(dave);
        fut.withdraw(id);
        assertEq(fut.treasuryOwed(), 1e18);
        assertEq(fut.held(), 0);
        _booksBalance();
    }

    function test_VoteDecidedFactNeverResolvesAMarket() public {
        uint256 id = _open();
        _bet(id, dave, 0, 3e18);
        _bet(id, erin, 1, 4e18);
        vm.warp(observable);
        uint256 c = _submit(sub, FACT, VAL, OnyxStakes.Kind.Voted);
        address v = makeAddr("v");
        _commit(c, v, 1, 30e18);
        _toReveal(c);
        _reveal(c, v, 1);
        _toResolve(c);
        stk.resolve(c);
        (bool fin,, uint8 level,) = stk.factValue(FACT);
        assertTrue(fin);
        assertEq(level, 4);
        vm.expectRevert(OnyxFutures.WrongPhase.selector);
        fut.resolve(id);
        vm.warp(deadline);
        fut.resolve(id);
        assertEq(uint8(fut.market(id).state), uint8(OnyxFutures.State.Void));
        assertEq(_fwithdraw(id, dave), 3e18);
        assertEq(_fwithdraw(id, erin), 4e18);
        _booksBalance();
    }

    /// THREATS STK-14: a vote-decided (level-4) answer must not lock a market's fact. A
    /// re-observed claim replaces it, and the market resolves from the re-observed value.
    function test_AVoteDecidedFactCanBeReobservedAndTheMarketResolves() public {
        uint256 id = _open();
        _bet(id, dave, 0, 3e18);
        _bet(id, erin, 1, 4e18);
        // A griefer gets a guessed value accepted by a cheap vote before the value exists.
        uint256 c = _submit(sub, FACT, VAL + 999_999, OnyxStakes.Kind.Voted);
        address v = makeAddr("v");
        _commit(c, v, 1, 30e18);
        _toReveal(c);
        _reveal(c, v, 1);
        _toResolve(c);
        stk.resolve(c);
        (,, uint8 level4,) = stk.factValue(FACT);
        assertEq(level4, 4);
        // After the value can be seen, an honest re-observed claim replaces the guess.
        vm.warp(observable);
        address honest = makeAddr("honest");
        _fund(honest, 10e18);
        uint256 h = _submit(honest, FACT, VAL, OnyxStakes.Kind.Observed);
        assertEq(uint8(stk.claim(h).state), uint8(OnyxStakes.State.Open));
        _allObserve(h, VAL);
        _toResolve(h);
        stk.resolve(h);
        (bool fin, int256 value, uint8 level,) = stk.factValue(FACT);
        assertTrue(fin);
        assertEq(value, VAL);
        assertEq(level, 2);
        fut.resolve(id);
        assertEq(uint8(fut.market(id).state), uint8(OnyxFutures.State.Resolved));
        // A level-2 answer is final: neither a vote nor another observation replaces it.
        vm.expectRevert(OnyxStakes.AlreadyFinal.selector);
        vm.prank(honest);
        stk.submit(FACT, VAL + 1, OnyxStakes.Kind.Observed);
        _booksBalance();
    }

    function test_AnAnswerAcceptedBeforeTheObservableTimeIsRefused() public {
        uint256 id = _open();
        _bet(id, erin, 1, 4e18);
        // Accepted before `observableAt`: cannot be the observation of a future value.
        _acceptObserved(VAL);
        vm.warp(observable);
        vm.expectRevert(OnyxFutures.WrongPhase.selector);
        fut.resolve(id);
        vm.warp(deadline);
        fut.resolve(id);
        assertEq(uint8(fut.market(id).state), uint8(OnyxFutures.State.Void));
    }

    function test_TradingMustCloseBeforeTheValueCanBeSeen() public {
        int256[] memory e = _edges();
        vm.expectRevert(OnyxFutures.BadMarket.selector);
        fut.createMarket(FACT, e, close, close + 59, deadline);
        vm.expectRevert(OnyxFutures.BadMarket.selector);
        fut.createMarket(FACT, e, uint64(block.timestamp), observable, deadline);
        vm.expectRevert(OnyxFutures.BadMarket.selector);
        fut.createMarket(FACT, e, close, observable, observable);
        int256[] memory bad = new int256[](2);
        (bad[0], bad[1]) = (5, 5);
        vm.expectRevert(OnyxFutures.BadMarket.selector);
        fut.createMarket(FACT, bad, close, observable, deadline);
        int256[] memory many = new int256[](8);
        for (uint256 i = 0; i < 8; i++) {
            many[i] = int256(i);
        }
        vm.expectRevert(OnyxFutures.BadMarket.selector);
        fut.createMarket(FACT, many, close, observable, deadline);
        vm.expectRevert(OnyxFutures.NotOwner.selector);
        vm.prank(dave);
        fut.createMarket(FACT, e, close, observable, deadline);
    }

    function test_ObserversAndTheArbiterMayNotBet() public {
        uint256 id = _open();
        _fund(obs2, 5e18);
        vm.expectRevert(OnyxFutures.NotAllowed.selector);
        vm.prank(obs2);
        fut.bet(id, 0, 1e18);
        vm.expectRevert(OnyxFutures.NotAllowed.selector);
        fut.bet(id, 0, 1e18);
    }

    function test_BetCaps() public {
        uint256 id = _open();
        _bet(id, dave, 0, 60e18);
        _fund(dave, 100e18);
        vm.expectRevert(OnyxFutures.BadAmount.selector);
        vm.prank(dave);
        fut.bet(id, 1, 41e18);
        vm.expectRevert(OnyxFutures.BadAmount.selector);
        vm.prank(dave);
        fut.bet(id, 1, 0);
        vm.expectRevert(OnyxFutures.BadMarket.selector);
        vm.prank(dave);
        fut.bet(id, 3, 1e18);
    }

    function test_EmptyWinningBucketVoidsAndRefunds() public {
        uint256 id = _open();
        _bet(id, dave, 0, 3e18);
        _bet(id, frank, 2, 2e18);
        vm.warp(observable);
        _acceptObserved(VAL);
        fut.resolve(id);
        assertEq(uint8(fut.market(id).state), uint8(OnyxFutures.State.Void));
        assertEq(_fwithdraw(id, dave), 3e18);
        assertEq(_fwithdraw(id, frank), 2e18);
        _booksBalance();
    }

    function test_BucketOfEdges() public view {
        int256[] memory e = _edges();
        assertEq(fut.bucketOf(e, 4_029_999_999_999_999), 0);
        assertEq(fut.bucketOf(e, 4_030_000_000_000_000), 1);
        assertEq(fut.bucketOf(e, VAL), 1);
        assertEq(fut.bucketOf(e, 4_031_000_000_000_000), 2);
    }

    /// Wash trading: one address on every bucket never gets more than it put in.
    function testFuzz_WashTradingNeverProfits(uint64[3] memory a, uint8 bucket, uint64 other)
        public
    {
        uint256 id = _open();
        address w = makeAddr("wash");
        uint256 put;
        for (uint8 i = 0; i < 3; i++) {
            uint128 x = uint128(bound(a[i], 1, 30e18));
            put += x;
            _bet(id, w, i, x);
        }
        _bet(id, dave, bucket % 3, uint128(bound(other, 1, 100e18)));
        vm.warp(observable);
        int256[3] memory vals = [int256(1), VAL, int256(5e15)];
        _acceptObserved(vals[bucket % 3 == 0 ? 0 : (bucket % 3 == 1 ? 1 : 2)]);
        fut.resolve(id);
        assertLe(_fwithdraw(id, w), put);
    }

    function testFuzz_MarketConservesTokens(uint64[5] memory a, uint8 buckets, uint8 pick) public {
        uint256 id = _open();
        uint256 total;
        address[5] memory who;
        for (uint256 i = 0; i < 5; i++) {
            who[i] = makeAddr(string(abi.encode("b", i)));
            uint128 x = uint128(bound(a[i], 1, 100e18));
            total += x;
            _bet(id, who[i], uint8((buckets >> i) % 3), x);
        }
        if (pick % 4 == 3) {
            vm.warp(deadline);
        } else {
            vm.warp(observable);
            int256[3] memory vals = [int256(1), VAL, int256(5e15)];
            _acceptObserved(vals[pick % 3]);
        }
        fut.resolve(id);
        uint256 out;
        for (uint256 i = 0; i < 5; i++) {
            if (fut.owed(id, who[i]) > 0) out += _fwithdraw(id, who[i]);
        }
        assertEq(out + fut.treasuryOwed(), total);
        assertEq(fut.held(), 0);
        _booksBalance();
    }
}
