// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";
import {OnyxStakes} from "../src/OnyxStakes.sol";
import {OnyxReobserve} from "../src/OnyxReobserve.sol";
import {OnyxTestnetOwnerOracle} from "../src/OnyxTestnetOwnerOracle.sol";
import {OnyxFutures} from "../src/OnyxFutures.sol";
import {
    IStakeToken,
    IOnyxFacts,
    IOnyxReobserve,
    IOnyxFallbackOracle,
    IOnyxFactGuard
} from "../src/IOnyxMarkets.sol";

/// The S3/S4/S6 contracts wired as `scripts/demo-markets.sh` deploys them (ADR-0042): the owner
/// deploys every contract and is the fallback arbiter; three observers; a treasury address.
contract OnyxMarketsFixture is Test {
    OnyxTestToken internal tok;
    OnyxReobserve internal ro;
    OnyxTestnetOwnerOracle internal orc;
    OnyxStakes internal stk;
    OnyxFutures internal fut;

    address internal owner = address(this);
    address internal treasury = address(0x7EA5);
    address internal sub = makeAddr("submitter");
    address internal obs1 = makeAddr("obs1");
    address internal obs2 = makeAddr("obs2");
    address internal obs3 = makeAddr("obs3");

    uint64 internal constant W = 100;
    bytes32 internal constant FACT = keccak256("fact one");
    int256 internal constant VAL = 4_030_531_621_082_972;

    function _stakeParams() internal pure returns (OnyxStakes.Params memory p) {
        p = OnyxStakes.Params({
            submitBond: 10e18,
            submitReward: 1e18,
            minStake: 1e18,
            maxStakePerAddress: 100e18,
            maxStakePerClaim: 1_000e18,
            quorum: 20e18,
            decisivePpm: 666_667,
            feePpm: 20_000,
            commitWindow: W,
            revealWindow: W,
            fallbackWindow: W
        });
    }

    function _futParams() internal pure returns (OnyxFutures.Params memory) {
        return OnyxFutures.Params({
            minBet: 1,
            maxBetPerAddress: 100e18,
            maxPoolPerMarket: 10_000e18,
            feePpm: 20_000,
            minCloseLead: 60,
            maxBuckets: 8,
            maxLevel: 2
        });
    }

    function setUp() public virtual {
        vm.warp(1_790_000_000);
        tok = new OnyxTestToken(owner);
        ro = new OnyxReobserve(0);
        orc = new OnyxTestnetOwnerOracle();
        stk = new OnyxStakes(
            IStakeToken(address(tok)),
            IOnyxReobserve(address(ro)),
            IOnyxFallbackOracle(address(orc)),
            treasury,
            _stakeParams()
        );
        fut = new OnyxFutures(
            IStakeToken(address(tok)),
            IOnyxFacts(address(stk)),
            IOnyxReobserve(address(ro)),
            IOnyxFallbackOracle(address(orc)),
            IOnyxFactGuard(address(0)),
            treasury,
            _futParams()
        );
        ro.bind(address(stk));
        orc.bind(address(stk));
        ro.addObserver(obs1);
        ro.addObserver(obs2);
        ro.addObserver(obs3);
        _fund(sub, 1_000e18);
    }

    function _fund(address who, uint256 amount) internal {
        tok.mint(who, amount);
        vm.startPrank(who);
        tok.approve(address(stk), type(uint256).max);
        tok.approve(address(fut), type(uint256).max);
        vm.stopPrank();
    }

    function _submit(address who, bytes32 fact, int256 v, OnyxStakes.Kind k)
        internal
        returns (uint256 id)
    {
        vm.prank(who);
        id = stk.submit(fact, v, k);
    }

    function _salt(address who) internal pure returns (bytes32) {
        return keccak256(abi.encode("salt", who));
    }

    function _commit(uint256 id, address who, uint8 side, uint128 amount) internal {
        if (tok.balanceOf(who) < amount) _fund(who, amount);
        bytes32 h = stk.commitHash(id, who, side, _salt(who));
        vm.prank(who);
        stk.commitStake(id, h, amount);
    }

    function _reveal(uint256 id, address who, uint8 side) internal {
        vm.prank(who);
        stk.revealStake(id, side, _salt(who));
    }

    function _toReveal(uint256 id) internal {
        vm.warp(stk.claim(id).commitClose);
    }

    function _toResolve(uint256 id) internal {
        vm.warp(stk.claim(id).revealClose);
    }

    function _observe(uint256 id, address o, uint8 status, int256 v) internal {
        bytes32 salt = keccak256(abi.encode("obs", o));
        bytes32 h = ro.commitHash(id, o, status, v, salt);
        vm.prank(o);
        ro.commit(id, h);
    }

    function _observeReveal(uint256 id, address o, uint8 status, int256 v) internal {
        bytes32 salt = keccak256(abi.encode("obs", o));
        vm.prank(o);
        ro.reveal(id, status, v, salt);
    }

    /// All three observers see `v` (commit now, reveal after the commit window).
    function _allObserve(uint256 id, int256 v) internal {
        _observe(id, obs1, 1, v);
        _observe(id, obs2, 1, v);
        _observe(id, obs3, 1, v);
        _toReveal(id);
        _observeReveal(id, obs1, 1, v);
        _observeReveal(id, obs2, 1, v);
        _observeReveal(id, obs3, 1, v);
    }

    function _withdraw(uint256 id, address who) internal returns (uint256 got) {
        uint256 b = tok.balanceOf(who);
        vm.prank(who);
        stk.withdraw(id);
        got = tok.balanceOf(who) - b;
    }

    /// Tokens conserved: the stake contract holds exactly its books.
    function _booksBalance() internal view {
        assertEq(
            tok.balanceOf(address(stk)),
            stk.held() + stk.treasuryOwed() + stk.rewardPool(),
            "stk books"
        );
        assertEq(tok.balanceOf(address(fut)), fut.held() + fut.treasuryOwed(), "fut books");
    }
}
