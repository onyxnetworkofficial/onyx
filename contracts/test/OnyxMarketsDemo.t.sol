// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {console2} from "forge-std/console2.sol";
import {OnyxMarketsFixture} from "./OnyxMarketsFixture.sol";
import {OnyxStakes} from "../src/OnyxStakes.sol";
import {OnyxFutures} from "../src/OnyxFutures.sol";

/// The whole S3/S4/S6 story in one run (ADR-0042; partner tracker T10), as the 2026-10-20 call
/// described it: a submitter stakes on data, others bet on it, observers re-read past data, a
/// contested claim goes to the fallback oracle, a duplicate earns nothing, and a futures market
/// on a future Treasury value pays from the re-observed answer. `scripts/demo-markets.sh` runs it
/// with `-vv` and the ledger it prints. Local EVM (Foundry), tONYX test token only.
contract OnyxMarketsDemoTest is OnyxMarketsFixture {
    address internal validator = makeAddr("validator");
    address internal challenger = makeAddr("challenger");
    address internal liar = makeAddr("wrong-submitter");
    address internal copier = makeAddr("duplicate-submitter");
    address internal ownData = makeAddr("own-data-submitter");
    address internal backer = makeAddr("backer");
    address internal doubter = makeAddr("doubter");
    address internal low = makeAddr("bettor-low");
    address internal mid = makeAddr("bettor-mid");
    address internal high = makeAddr("bettor-high");

    bytes32 internal constant DEBT_PAST = keccak256("treasury.debt_to_penny 2026-10-19");
    bytes32 internal constant DEBT_WRONG = keccak256("treasury.debt_to_penny 2026-10-16");
    bytes32 internal constant OWN = keccak256("own-data.example 2026-10-20");
    bytes32 internal constant DEBT_FUTURE = keccak256("treasury.debt_to_penny 2026-11-02");

    address[] internal actors;
    mapping(address => int256) internal net;

    function _actor(address a, uint256 funds) internal {
        actors.push(a);
        _fund(a, funds);
    }

    function _take(uint256 id, address who) internal {
        uint256 got = _withdraw(id, who);
        net[who] += int256(got);
    }

    function _paid(address who, uint256 amount) internal {
        net[who] -= int256(amount);
    }

    function _fmt(int256 v) internal pure returns (string memory) {
        bool neg = v < 0;
        uint256 u = uint256(neg ? -v : v);
        uint256 cents = (u % 1e18) / 1e16;
        string memory frac =
            cents < 10 ? string.concat("0", vm.toString(cents)) : vm.toString(cents);
        return string.concat(neg ? "-" : "+", vm.toString(u / 1e18), ".", frac);
    }

    function _stake(uint256 id, address who, uint8 side, uint128 amount) internal {
        _commit(id, who, side, amount);
        _paid(who, amount);
    }

    function test_DemoStory() public {
        _actor(validator, 100e18);
        _actor(challenger, 100e18);
        _actor(liar, 100e18);
        _actor(copier, 100e18);
        _actor(ownData, 100e18);
        _actor(backer, 100e18);
        _actor(doubter, 100e18);
        _actor(low, 100e18);
        _actor(mid, 100e18);
        _actor(high, 100e18);
        actors.push(sub);
        tok.mint(address(this), 100e18);
        tok.approve(address(stk), type(uint256).max);
        stk.fundRewards(100e18);

        // 1. An honest submitter stakes 10 on a past Treasury value; a validator backs it with 20.
        //    Observers re-read the source and agree: accepted at level 2, everyone paid.
        console2.log("1. Honest past-data submission, re-observed by 3 observers");
        uint256 c1 = _submit(sub, DEBT_PAST, VAL, OnyxStakes.Kind.Observed);
        _paid(sub, stk.claim(c1).bond);
        _stake(c1, validator, 1, 20e18);
        _allObserve(c1, VAL);
        _reveal(c1, validator, 1);
        _toResolve(c1);
        stk.resolve(c1);
        require(stk.claim(c1).state == OnyxStakes.State.Correct, "c1 correct");
        _take(c1, sub);
        _take(c1, validator);

        // 2. Somebody submits the same value again: a duplicate takes nothing and earns nothing.
        console2.log("2. Duplicate of an accepted value: nothing taken, nothing paid");
        uint256 c2 = _submit(copier, DEBT_PAST, VAL, OnyxStakes.Kind.Observed);
        require(stk.claim(c2).state == OnyxStakes.State.Duplicate, "c2 duplicate");

        // 3. A wrong value: a challenger stakes 20 that it is wrong; observers see the real value.
        //    The submitter loses its bond; the challenger wins it less the fee.
        console2.log("3. Wrong submission: slashed by the re-observation");
        uint256 c3 = _submit(liar, DEBT_WRONG, VAL + 45, OnyxStakes.Kind.Observed);
        _paid(liar, stk.claim(c3).bond);
        _stake(c3, challenger, 2, 20e18);
        _allObserve(c3, VAL);
        _reveal(c3, challenger, 2);
        _toResolve(c3);
        stk.resolve(c3);
        require(stk.claim(c3).state == OnyxStakes.State.Wrong, "c3 wrong");
        _take(c3, challenger);

        // 4. Own data nobody can re-read: only the stake vote can decide. It splits 15 to 15, so it
        //    is contested and goes to the fallback oracle (testnet: the owner), which says wrong.
        console2.log("4. Own data, contested vote (15 v 15): fallback oracle decides");
        uint256 c4 = _submit(ownData, OWN, 7, OnyxStakes.Kind.Voted);
        _paid(ownData, stk.claim(c4).bond);
        _stake(c4, backer, 1, 15e18);
        _stake(c4, doubter, 2, 15e18);
        _toReveal(c4);
        _reveal(c4, backer, 1);
        _reveal(c4, doubter, 2);
        _toResolve(c4);
        stk.resolve(c4);
        require(stk.claim(c4).state == OnyxStakes.State.Fallback, "c4 fallback");
        orc.answer(c4, 2);
        stk.settleFallback(c4);
        require(stk.claim(c4).state == OnyxStakes.State.Wrong, "c4 wrong");
        _take(c4, doubter);

        // 5. A futures market on a FUTURE Treasury value: below / near / above the expected value.
        //    Trading closes before the value exists; it resolves only from the re-observed answer.
        console2.log("5. Futures market on a future Treasury value, 3 buckets");
        int256[] memory edges = new int256[](2);
        edges[0] = VAL - 1_000;
        edges[1] = VAL + 1_000;
        uint64 close = uint64(block.timestamp + 1_000);
        uint64 observable = close + 60;
        uint256 m = fut.createMarket(DEBT_FUTURE, edges, close, observable, observable + 10_000);
        vm.prank(low);
        fut.bet(m, 0, 30e18);
        _paid(low, 30e18);
        vm.prank(mid);
        fut.bet(m, 1, 50e18);
        _paid(mid, 50e18);
        vm.prank(high);
        fut.bet(m, 2, 20e18);
        _paid(high, 20e18);
        vm.warp(observable);
        uint256 c5 = _submit(sub, DEBT_FUTURE, VAL + 12, OnyxStakes.Kind.Observed);
        _paid(sub, stk.claim(c5).bond);
        _allObserve(c5, VAL + 12);
        _toResolve(c5);
        stk.resolve(c5);
        _take(c5, sub);
        fut.resolve(m);
        require(fut.market(m).state == OnyxFutures.State.Resolved, "market resolved");
        uint256 before = tok.balanceOf(mid);
        vm.prank(mid);
        fut.withdraw(m);
        net[mid] += int256(tok.balanceOf(mid) - before);

        _booksBalance();
        console2.log("");
        console2.log("Ledger (tONYX test tokens, net per actor):");
        for (uint256 i = 0; i < actors.length; i++) {
            console2.log(string.concat("  ", vm.getLabel(actors[i]), " ", _fmt(net[actors[i]])));
        }
        console2.log(
            string.concat(
                "  treasury owed ",
                _fmt(int256(stk.treasuryOwed() + fut.treasuryOwed())),
                "; reward pool left ",
                _fmt(int256(stk.rewardPool()))
            )
        );
        console2.log("Books balance: every token the contracts hold is accounted for.");
    }
}
