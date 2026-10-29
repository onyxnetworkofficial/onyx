// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test, console} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {OnyxTestToken} from "../../src/OnyxTestToken.sol";
import {OnyxStakes} from "../../src/OnyxStakes.sol";
import {OnyxReobserve} from "../../src/OnyxReobserve.sol";
import {OnyxTestnetOwnerOracle} from "../../src/OnyxTestnetOwnerOracle.sol";
import {OnyxFutures} from "../../src/OnyxFutures.sol";
import {
    IStakeToken,
    IOnyxFacts,
    IOnyxReobserve,
    IOnyxFallbackOracle,
    IOnyxFactGuard
} from "../../src/IOnyxMarkets.sol";

/// Random submissions, stakes, reveals, observations, fallbacks, bets, resolutions and
/// withdrawals over a few facts and actors (ADR-0042 item 15). The handler owns every contract,
/// so it is the fallback arbiter and the market creator.
contract MarketsHandler is Test {
    OnyxTestToken public tok;
    OnyxReobserve public ro;
    OnyxTestnetOwnerOracle public orc;
    OnyxStakes public stk;
    OnyxFutures public fut;
    address[6] internal actors;
    address[3] internal observers;
    bytes32[3] internal factIds;
    int256[3] internal values = [int256(7), int256(8), int256(9)];
    uint256 public minted;
    /// Successful calls, for coverage (`ONYX_INVARIANT_STATS=1`).
    uint256 public decided;
    uint256 public paidOut;
    uint256 public marketsDone;

    constructor() {
        tok = new OnyxTestToken(address(this));
        ro = new OnyxReobserve(0);
        orc = new OnyxTestnetOwnerOracle();
        OnyxStakes.Params memory p = OnyxStakes.Params({
            submitBond: 10e18,
            submitReward: 1e18,
            minStake: 1e18,
            maxStakePerAddress: 100e18,
            maxStakePerClaim: 1_000e18,
            quorum: 20e18,
            decisivePpm: 666_667,
            feePpm: 37_000,
            commitWindow: 100,
            revealWindow: 100,
            fallbackWindow: 100
        });
        stk = new OnyxStakes(
            IStakeToken(address(tok)),
            IOnyxReobserve(address(ro)),
            IOnyxFallbackOracle(address(orc)),
            address(0x7EA5),
            p
        );
        fut = new OnyxFutures(
            IStakeToken(address(tok)),
            IOnyxFacts(address(stk)),
            IOnyxReobserve(address(ro)),
            IOnyxFallbackOracle(address(orc)),
            IOnyxFactGuard(address(0)),
            address(0x7EA5),
            OnyxFutures.Params({
                minBet: 1,
                maxBetPerAddress: 100e18,
                maxPoolPerMarket: 10_000e18,
                feePpm: 37_000,
                minCloseLead: 60,
                maxBuckets: 8,
                maxLevel: 2
            })
        );
        ro.bind(address(stk));
        orc.bind(address(stk));
        for (uint256 i = 0; i < 3; i++) {
            observers[i] = makeAddr(string(abi.encode("obs", i)));
            ro.addObserver(observers[i]);
            factIds[i] = keccak256(abi.encode("fact", i));
        }
        for (uint256 i = 0; i < 6; i++) {
            actors[i] = makeAddr(string(abi.encode("actor", i)));
            tok.mint(actors[i], 10_000e18);
            minted += 10_000e18;
            vm.startPrank(actors[i]);
            tok.approve(address(stk), type(uint256).max);
            tok.approve(address(fut), type(uint256).max);
            vm.stopPrank();
        }
        tok.mint(address(this), 100e18);
        minted += 100e18;
        tok.approve(address(stk), type(uint256).max);
        stk.fundRewards(100e18);
    }

    function _claim(uint256 seed) internal view returns (uint256) {
        uint256 n = stk.claimCount();
        return n == 0 ? 0 : 1 + seed % n;
    }

    function submit(uint8 a, uint8 f, uint8 v, bool observed) external {
        vm.prank(actors[a % 6]);
        try stk.submit(
            factIds[f % 3],
            values[v % 3],
            observed ? OnyxStakes.Kind.Observed : OnyxStakes.Kind.Voted
        ) {}
            catch {}
    }

    function commit(uint8 a, uint256 c, uint8 side, uint128 amount) external {
        uint256 id = _claim(c);
        address who = actors[a % 6];
        amount = uint128(bound(amount, 1e18, 100e18));
        bytes32 h = stk.commitHash(id, who, 1 + side % 2, bytes32(uint256(a)));
        vm.prank(who);
        try stk.commitStake(id, h, amount) {} catch {}
    }

    function reveal(uint8 a, uint256 c, uint8 side) external {
        uint256 id = _claim(c);
        vm.prank(actors[a % 6]);
        try stk.revealStake(id, 1 + side % 2, bytes32(uint256(a))) {} catch {}
    }

    function observe(uint8 o, uint256 c, uint8 v) external {
        uint256 id = _claim(c);
        address who = observers[o % 3];
        bytes32 h = ro.commitHash(id, who, 1, values[v % 3], bytes32(uint256(v % 3)));
        vm.prank(who);
        try ro.commit(id, h) {} catch {}
        vm.prank(who);
        try ro.reveal(id, 1, values[v % 3], bytes32(uint256(v % 3))) {} catch {}
    }

    function warp(uint16 dt) external {
        vm.warp(block.timestamp + bound(dt, 1, 300));
    }

    function resolve(uint256 c) external {
        try stk.resolve(_claim(c)) {
            decided++;
        } catch {}
    }

    function answer(uint256 c, uint8 a) external {
        uint256 id = _claim(c);
        try orc.answer(id, 1 + a % 3) {} catch {}
        try stk.settleFallback(id) {
            decided++;
        } catch {}
    }

    function withdraw(uint8 a) external {
        // Every actor on every claim, so decided claims are paid out often.
        uint256 n = stk.claimCount();
        for (uint256 id = 1; id <= n; id++) {
            for (uint256 i = 0; i < 6; i++) {
                if (stk.owed(id, actors[(a + i) % 6]) == 0) continue;
                vm.prank(actors[(a + i) % 6]);
                stk.withdraw(id);
                paidOut++;
            }
        }
    }

    function payTreasury(bool which) external {
        if (which) {
            try stk.payTreasury() {} catch {}
        } else {
            try fut.payTreasury() {} catch {}
        }
    }

    function openMarket(uint8 f, int64 edge) external {
        int256[] memory e = new int256[](1);
        e[0] = int256(edge % 10);
        uint64 close = uint64(block.timestamp) + 50;
        try fut.createMarket(factIds[f % 3], e, close, close + 60, close + 600) {} catch {}
    }

    function bet(uint8 a, uint256 m, uint8 b, uint128 amount) external {
        uint256 n = fut.marketCount();
        if (n == 0) return;
        vm.prank(actors[a % 6]);
        try fut.bet(1 + m % n, b % 2, uint128(bound(amount, 1, 100e18))) {} catch {}
    }

    function settleMarket(uint8 a, uint256 m) external {
        uint256 n = fut.marketCount();
        if (n == 0) return;
        try fut.resolve(1 + m % n) {
            marketsDone++;
        } catch {}
        vm.prank(actors[a % 6]);
        try fut.withdraw(1 + m % n) {} catch {}
    }
}

contract MarketsInvariantsTest is StdInvariant, Test {
    MarketsHandler internal h;

    function setUp() public {
        vm.warp(1_790_000_000);
        h = new MarketsHandler();
        targetContract(address(h));
    }

    /// Each contract holds exactly what its books say it owes.
    function invariant_BooksMatchBalances() public view {
        OnyxStakes s = h.stk();
        OnyxFutures f = h.fut();
        assertEq(h.tok().balanceOf(address(s)), s.held() + s.treasuryOwed() + s.rewardPool());
        assertEq(h.tok().balanceOf(address(f)), f.held() + f.treasuryOwed());
    }

    function afterInvariant() public view {
        if (vm.envOr("ONYX_INVARIANT_STATS", uint256(0)) == 1) {
            console.log("decided", h.decided(), "paid", h.paidOut());
            console.log("markets done", h.marketsDone());
        }
    }

    /// No token is created or destroyed: the supply equals what was minted.
    function invariant_SupplyIsMinted() public view {
        assertEq(h.tok().totalSupply(), h.minted());
    }
}

contract MarketsHandlerSmokeTest is Test {
    function test_HandlerReachesDecisionsAndPayouts() public {
        vm.warp(1_790_000_000);
        MarketsHandler h = new MarketsHandler();
        h.openMarket(0, 3);
        h.bet(1, 0, 0, 5e18);
        h.bet(2, 0, 1, 7e18);
        h.submit(0, 0, 0, true);
        h.commit(1, 0, 0, 30e18);
        h.observe(0, 0, 0);
        h.observe(1, 0, 0);
        h.observe(2, 0, 0);
        h.warp(150);
        h.observe(0, 0, 0);
        h.observe(1, 0, 0);
        h.observe(2, 0, 0);
        h.reveal(1, 0, 0);
        h.warp(150);
        h.resolve(0);
        h.withdraw(0);
        h.warp(300);
        h.settleMarket(1, 0);
        assertEq(h.decided(), 1, "decided");
        assertEq(h.paidOut(), 2, "paid");
        assertEq(h.marketsDone(), 1, "markets");
    }
}
