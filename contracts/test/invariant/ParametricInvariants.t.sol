// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {StdInvariant} from "forge-std/StdInvariant.sol";
import {OnyxTestToken} from "../../src/OnyxTestToken.sol";
import {OnyxReobserve} from "../../src/OnyxReobserve.sol";
import {OnyxParametric} from "../../src/OnyxParametric.sol";
import {IStakeToken, IOnyxFacts, IOnyxFactGuard, IOnyxReobserve} from "../../src/IOnyxMarkets.sol";
import {SetFacts} from "../OnyxParametric.t.sol";

/// Random deposits, withdrawals, offers, purchases, values, settlements and claims over a few
/// products and actors (NL18, ADR-0042 amendment 3). The handler owns the contract and the facts source.
contract ParametricHandler is Test {
    OnyxTestToken public tok;
    SetFacts public sf;
    OnyxParametric public ins;
    address[5] internal actors;
    uint256 public settled;
    uint256 public claimedCount;

    constructor() {
        tok = new OnyxTestToken(address(this));
        sf = new SetFacts(address(0));
        ins = new OnyxParametric(
            IStakeToken(address(tok)),
            IOnyxFacts(address(sf)),
            IOnyxFactGuard(address(sf)),
            IOnyxReobserve(address(new OnyxReobserve(0)))
        );
        for (uint256 i = 0; i < actors.length; i++) {
            actors[i] = address(uint160(0xA0 + i));
            vm.prank(actors[i]);
            tok.approve(address(ins), type(uint256).max);
        }
    }

    function _actor(uint256 s) internal view returns (address) {
        return actors[s % actors.length];
    }

    function underwrite(uint256 who, uint256 amount) external {
        amount = bound(amount, 1, 1_000e18);
        address a = _actor(who);
        tok.mint(a, amount);
        vm.prank(a);
        ins.underwrite(amount);
    }

    function withdraw(uint256 who, uint256 part) external {
        address a = _actor(who);
        uint256 s = ins.shares(a);
        if (s == 0) return;
        s = bound(part, 1, s);
        vm.prank(a);
        try ins.withdraw(s) {} catch {}
    }

    function offer(uint256 seed, int256 threshold, uint128 payout, uint128 premium) external {
        bytes32 fact = keccak256(abi.encode(seed % 4));
        uint64 buyBy = uint64(block.timestamp) + 1 days;
        if (sf.closeBy(fact) == 0) sf.listUntil(fact, buyBy);
        payout = uint128(bound(payout, 2, 500e18));
        premium = uint128(bound(premium, 1, payout - 1));
        threshold = bound(threshold, -10, 10);
        try ins.offer(
            fact,
            OnyxParametric.Cmp(seed % 2),
            threshold,
            payout,
            premium,
            sf.closeBy(fact),
            sf.closeBy(fact) + 2 hours,
            sf.closeBy(fact) + 4 days,
            20
        ) {}
            catch {}
    }

    function buy(uint256 who, uint256 id, uint32 n) external {
        if (ins.productCount() == 0) return;
        id = bound(id, 1, ins.productCount());
        n = uint32(bound(n, 1, 3));
        address a = _actor(who);
        tok.mint(a, uint256(ins.product(id).premium) * n);
        vm.prank(a);
        try ins.buy(id, n) {} catch {}
    }

    function setValue(uint256 seed, int256 value, uint8 level) external {
        value = bound(value, -10, 10);
        level = uint8(bound(level, 1, 4));
        sf.set(keccak256(abi.encode(seed % 4)), value, level);
    }

    function warp(uint256 secs) external {
        vm.warp(block.timestamp + bound(secs, 1, 3 days));
    }

    function settle(uint256 id) external {
        if (ins.productCount() == 0) return;
        id = bound(id, 1, ins.productCount());
        try ins.settle(id) {
            settled++;
        } catch {}
    }

    function claim(uint256 who, uint256 id) external {
        if (ins.productCount() == 0) return;
        id = bound(id, 1, ins.productCount());
        vm.prank(_actor(who));
        try ins.claim(id) {
            claimedCount++;
        } catch {}
    }
}

contract ParametricInvariants is StdInvariant, Test {
    ParametricHandler internal h;

    function setUp() public {
        vm.warp(1_800_000_000);
        h = new ParametricHandler();
        targetContract(address(h));
    }

    /// Solvent: the contract holds every token it owes (pool, unsettled premiums, claims).
    function invariant_TheContractHoldsWhatItOwes() public view {
        OnyxParametric ins = h.ins();
        assertGe(h.tok().balanceOf(address(ins)), ins.liabilities());
    }

    /// Every sold, unsettled policy's payout is covered by the pool.
    function invariant_ReservesNeverExceedThePool() public view {
        OnyxParametric ins = h.ins();
        assertLe(ins.reserved(), ins.pool());
    }
}
