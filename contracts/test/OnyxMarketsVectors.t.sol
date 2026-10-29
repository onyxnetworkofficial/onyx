// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {OnyxMarketsFixture} from "./OnyxMarketsFixture.sol";
import {OnyxStakes} from "../src/OnyxStakes.sol";
import {OnyxFutures} from "../src/OnyxFutures.sol";
import {OnyxPayout} from "../src/OnyxPayout.sol";
import {
    IStakeToken,
    IOnyxFacts,
    IOnyxReobserve,
    IOnyxFallbackOracle,
    IOnyxFactGuard
} from "../src/IOnyxMarkets.sol";

/// A fact source the vector test sets directly.
contract VectorFacts is IOnyxFacts {
    bool internal fin;
    int256 internal v;
    uint64 internal at;

    function set(int256 value, uint64 when) external {
        (fin, v, at) = (true, value, when);
    }

    function factValue(bytes32) external view returns (bool, int256, uint8, uint64) {
        return (fin, v, 2, at);
    }
}

/// fixtures/markets/vectors.json through the real contracts (ADR-0042 item 15): the same file
/// Rust (`onyx-markets`) and the website (`markets.ts`) run.
contract OnyxMarketsVectorsTest is OnyxMarketsFixture {
    string internal json;

    function setUp() public override {
        super.setUp();
        json = vm.readFile("../fixtures/markets/vectors.json");
    }

    function _k(string memory base, uint256 i, string memory rest)
        internal
        view
        returns (string memory)
    {
        return string.concat(base, "[", vm.toString(i), "]", rest);
    }

    function _side(string memory s) internal pure returns (uint8) {
        if (keccak256(bytes(s)) == keccak256("correct")) return 1;
        if (keccak256(bytes(s)) == keccak256("wrong")) return 2;
        return 0;
    }

    function test_ClaimVectors() public {
        for (uint256 i = 0; vm.keyExistsJson(json, _k(".claims", i, "")); i++) {
            _claimCase(i);
        }
    }

    function _claimCase(uint256 i) internal {
        setUp();
        string memory b = _k(".claims", i, "");
        OnyxStakes.Params memory p = _stakeParams();
        p.submitBond = uint128(vm.parseJsonUint(json, string.concat(b, ".bond")));
        p.feePpm = uint32(vm.parseJsonUint(json, string.concat(b, ".feePpm")));
        p.submitReward = uint128(vm.parseJsonUint(json, string.concat(b, ".submitReward")));
        p.minStake = 1;
        p.maxStakePerAddress = type(uint128).max / 4;
        p.maxStakePerClaim = type(uint128).max / 2;
        p.quorum = type(uint128).max;
        stk.setParams(p);
        uint256 pool = vm.parseJsonUint(json, string.concat(b, ".rewardPool"));
        _fund(address(this), pool);
        if (pool > 0) stk.fundRewards(pool);
        _fund(sub, p.submitBond);
        uint256 id = _submit(sub, keccak256(abi.encode("vec", i)), 1, OnyxStakes.Kind.Voted);
        uint256 n;
        while (vm.keyExistsJson(json, string.concat(b, ".stakes[", vm.toString(n), "]"))) n++;
        address[] memory who = new address[](n);
        uint8[] memory side = new uint8[](n);
        for (uint256 j = 0; j < n; j++) {
            string memory s = string.concat(b, ".stakes[", vm.toString(j), "]");
            who[j] = makeAddr(vm.parseJsonString(json, string.concat(s, ".who")));
            side[j] = _side(vm.parseJsonString(json, string.concat(s, ".side")));
            uint128 amt = uint128(vm.parseJsonUint(json, string.concat(s, ".amount")));
            // An unrevealed stake commits to "correct" and never reveals.
            _commit(id, who[j], side[j] == 0 ? 1 : side[j], amt);
        }
        _toReveal(id);
        for (uint256 j = 0; j < n; j++) {
            if (side[j] != 0) _reveal(id, who[j], side[j]);
        }
        _toResolve(id);
        stk.resolve(id);
        string memory outcome = vm.parseJsonString(json, string.concat(b, ".outcome"));
        uint8 ans = keccak256(bytes(outcome)) == keccak256("correct")
            ? 1
            : (keccak256(bytes(outcome)) == keccak256("wrong") ? 2 : 3);
        orc.answer(id, ans);
        stk.settleFallback(id);
        string memory e = string.concat(b, ".expect");
        uint256 treasuryAtDecide = stk.treasuryOwed();
        uint256 got = stk.owed(id, sub) > 0 ? _withdraw(id, sub) : 0;
        assertEq(got, vm.parseJsonUint(json, string.concat(e, ".payouts.submitter")), "submitter");
        for (uint256 j = 0; j < n; j++) {
            string memory name =
                vm.parseJsonString(json, string.concat(b, ".stakes[", vm.toString(j), "].who"));
            got = stk.owed(id, who[j]) > 0 ? _withdraw(id, who[j]) : 0;
            assertEq(got, vm.parseJsonUint(json, string.concat(e, ".payouts.", name)), name);
        }
        assertEq(
            stk.treasuryOwed(), vm.parseJsonUint(json, string.concat(e, ".treasury")), "treasury"
        );
        assertEq(
            stk.treasuryOwed() - treasuryAtDecide,
            vm.parseJsonUint(json, string.concat(e, ".dust")),
            "dust"
        );
        assertEq(stk.held(), 0);
        _booksBalance();
    }

    function test_VoteVectors() public view {
        for (uint256 i = 0; vm.keyExistsJson(json, _k(".votes", i, "")); i++) {
            string memory b = _k(".votes", i, "");
            uint8 got = stk.voteOutcome(
                vm.parseJsonUint(json, string.concat(b, ".correct")),
                vm.parseJsonUint(json, string.concat(b, ".wrong")),
                vm.parseJsonUint(json, string.concat(b, ".quorum")),
                vm.parseJsonUint(json, string.concat(b, ".decisivePpm"))
            );
            string memory want = vm.parseJsonString(json, string.concat(b, ".expect"));
            assertEq(got, _side(want), want);
        }
    }

    function test_AttackVectors() public view {
        string memory j = vm.readFile("../fixtures/markets/vectors.json");
        for (
            uint256 i = 0;
            vm.keyExistsJson(j, string.concat(".attack[", vm.toString(i), "]"));
            i++
        ) {
            string memory b = string.concat(".attack[", vm.toString(i), "]");
            uint256 a = OnyxPayout.minAttackStake(
                vm.parseJsonUint(j, string.concat(b, ".honest")),
                vm.parseJsonUint(j, string.concat(b, ".quorum")),
                vm.parseJsonUint(j, string.concat(b, ".decisivePpm")),
                vm.parseJsonUint(j, string.concat(b, ".minStake")),
                vm.parseJsonUint(j, string.concat(b, ".maxPerClaim"))
            );
            assertEq(a, vm.parseJsonUint(j, string.concat(b, ".expect")));
        }
    }

    function test_BucketVectors() public view {
        for (uint256 i = 0; vm.keyExistsJson(json, _k(".buckets", i, "")); i++) {
            string memory b = _k(".buckets", i, "");
            int256[] memory e = vm.parseJsonIntArray(json, string.concat(b, ".edges"));
            int256 v = vm.parseJsonInt(json, string.concat(b, ".value"));
            assertEq(fut.bucketOf(e, v), vm.parseJsonUint(json, string.concat(b, ".bucket")));
        }
    }

    function test_MarketVectors() public {
        for (uint256 i = 0; vm.keyExistsJson(json, _k(".markets", i, "")); i++) {
            _marketCase(i);
        }
    }

    function _marketCase(uint256 i) internal {
        setUp();
        string memory b = _k(".markets", i, "");
        VectorFacts facts = new VectorFacts();
        OnyxFutures.Params memory p = _futParams();
        p.feePpm = uint32(vm.parseJsonUint(json, string.concat(b, ".feePpm")));
        OnyxFutures f = new OnyxFutures(
            IStakeToken(address(tok)),
            IOnyxFacts(address(facts)),
            IOnyxReobserve(address(ro)),
            IOnyxFallbackOracle(address(orc)),
            IOnyxFactGuard(address(0)),
            treasury,
            p
        );
        uint64 close = uint64(block.timestamp) + 100;
        uint256 id = f.createMarket(
            FACT,
            vm.parseJsonIntArray(json, string.concat(b, ".edges")),
            close,
            close + 60,
            close + 200
        );
        for (
            uint256 j = 0;
            vm.keyExistsJson(json, string.concat(b, ".bets[", vm.toString(j), "]"));
            j++
        ) {
            string memory s = string.concat(b, ".bets[", vm.toString(j), "]");
            address w = makeAddr(vm.parseJsonString(json, string.concat(s, ".who")));
            uint128 amt = uint128(vm.parseJsonUint(json, string.concat(s, ".amount")));
            tok.mint(w, amt);
            vm.startPrank(w);
            tok.approve(address(f), amt);
            f.bet(id, uint8(vm.parseJsonUint(json, string.concat(s, ".bucket"))), amt);
            vm.stopPrank();
        }
        // A null value means no answer ever comes: void at the deadline.
        bool hasValue = _valueIsString(b);
        if (hasValue) {
            vm.warp(close + 60);
            facts.set(vm.parseJsonInt(json, string.concat(b, ".value")), close + 60);
        } else {
            vm.warp(close + 200);
        }
        f.resolve(id);
        OnyxFutures.Market memory m = f.market(id);
        string memory e = string.concat(b, ".expect");
        bool resolved = keccak256(bytes(vm.parseJsonString(json, string.concat(e, ".state"))))
            == keccak256("resolved");
        assertEq(
            uint8(m.state),
            uint8(resolved ? OnyxFutures.State.Resolved : OnyxFutures.State.Void),
            vm.parseJsonString(json, string.concat(b, ".name"))
        );
        if (resolved) assertEq(m.winner, vm.parseJsonUint(json, string.concat(e, ".winner")));
        uint256 treasuryAtDecide = f.treasuryOwed();
        assertEq(treasuryAtDecide, vm.parseJsonUint(json, string.concat(e, ".fee")), "fee");
        string[] memory names = vm.parseJsonKeys(json, string.concat(e, ".payouts"));
        for (uint256 j = 0; j < names.length; j++) {
            address w = makeAddr(names[j]);
            uint256 got;
            if (f.owed(id, w) > 0) {
                uint256 before = tok.balanceOf(w);
                vm.prank(w);
                f.withdraw(id);
                got = tok.balanceOf(w) - before;
            }
            assertEq(got, vm.parseJsonUint(json, string.concat(e, ".payouts.", names[j])), names[j]);
        }
        assertEq(
            f.treasuryOwed(), vm.parseJsonUint(json, string.concat(e, ".treasury")), "treasury"
        );
        assertEq(f.held(), 0);
        assertEq(tok.balanceOf(address(f)), f.treasuryOwed());
    }

    function _valueIsString(string memory b) internal view returns (bool) {
        try this.parseValue(b) returns (int256) {
            return true;
        } catch {
            return false;
        }
    }

    function parseValue(string memory b) external view returns (int256) {
        return vm.parseJsonInt(json, string.concat(b, ".value"));
    }
}
