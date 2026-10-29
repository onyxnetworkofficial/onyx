// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxRegistry, IRegistryLedger} from "../src/OnyxRegistry.sol";
import {OnyxObservedFacts} from "../src/OnyxObservedFacts.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";
import {OnyxReobserve} from "../src/OnyxReobserve.sol";
import {OnyxParametric} from "../src/OnyxParametric.sol";
import {IStakeToken, IOnyxFacts, IOnyxFactGuard, IOnyxReobserve} from "../src/IOnyxMarkets.sol";
import {CoordinatorLedger} from "./OnyxObservedFacts.t.sol";

/// A facts source whose values, levels and listings the test sets.
contract SetFacts is IOnyxFacts, IOnyxFactGuard {
    struct F {
        bool fin;
        int256 value;
        uint8 level;
        uint64 at;
    }

    mapping(bytes32 => F) public f;
    mapping(bytes32 => uint64) public closeBy;
    address public insider;

    constructor(address insider_) {
        insider = insider_;
    }

    function listUntil(bytes32 fact, uint64 c) external {
        closeBy[fact] = c;
    }

    function set(bytes32 fact, int256 value, uint8 level) external {
        f[fact] = F(true, value, level, uint64(block.timestamp));
    }

    function factValue(bytes32 fact) external view returns (bool, int256, uint8, uint64) {
        F memory x = f[fact];
        return (x.fin, x.value, x.level, x.at);
    }

    function isInsider(address who) external view returns (bool) {
        return who == insider;
    }
}

/// NL18 (ADR-0042 amendment 3): parametric policies settled from a level-2 value the registry recorded.
contract OnyxParametricTest is Test {
    OnyxRegistry internal reg;
    OnyxObservedFacts internal facts;
    OnyxTestToken internal tok;
    OnyxReobserve internal ro;
    OnyxParametric internal ins;

    address internal gov = address(0x60F);
    address internal uma = makeAddr("uma");
    address internal vic = makeAddr("vic");
    address internal bob = makeAddr("bob");
    address internal cat = makeAddr("cat");

    bytes32 internal constant METRIC = keccak256("noaa.ghcnd.usw00003017.prcp");
    bytes32 internal constant TEMPLATE = keccak256("prcp template");
    bytes32 internal constant SUBJECT = keccak256("noaa.ghcnd.usw00003017:2026-10-30");
    uint64 internal constant WINDOW = 1_793_250_000;
    bytes32 internal constant POOL = keccak256("pool");
    bytes32 internal constant CFG = keccak256("cfg");
    bytes32 internal constant SPEC = keccak256("spec");
    bytes32 internal constant ROUND = keccak256("round");

    uint64 internal buyBy;
    uint64 internal observable;
    uint64 internal deadline;

    function setUp() public {
        vm.warp(1_793_100_000);
        vm.roll(1_000);
        reg = new OnyxRegistry(gov, IRegistryLedger(address(new CoordinatorLedger(address(this)))));
        vm.prank(gov);
        reg.registerMetric(METRIC, OnyxRegistry.Kind.DatedFeed, TEMPLATE, 0);
        reg.setReceiptSigner(address(0x5EC));
        facts = new OnyxObservedFacts(reg, address(this));
        tok = new OnyxTestToken(address(this));
        ro = new OnyxReobserve(0);
        ins = new OnyxParametric(
            IStakeToken(address(tok)),
            IOnyxFacts(address(facts)),
            IOnyxFactGuard(address(facts)),
            IOnyxReobserve(address(ro))
        );
        buyBy = uint64(block.timestamp) + 1 days;
        observable = buyBy + 12 hours;
        deadline = observable + 7 days;
    }

    function _list() internal returns (bytes32) {
        return facts.list(METRIC, TEMPLATE, SUBJECT, 0, true, uint64(block.number) + 10, buyBy);
    }

    function _record(int128 value) internal {
        bytes32 fact = reg.factKey(METRIC, SUBJECT, WINDOW);
        uint32 n = reg.drawCount(fact);
        uint64 h = uint64(vm.getBlockNumber()) + 1;
        reg.announceDraw(METRIC, SUBJECT, WINDOW, n, reg.drawCommit(POOL, SPEC, CFG), h);
        vm.roll(h + 1);
        vm.setBlockhash(h, keccak256(abi.encode("beacon", fact, n)));
        reg.fixBeacon(fact, n);
        reg.recordObservation(
            METRIC, SUBJECT, WINDOW, SPEC, facts.resultHash(SPEC, ROUND, value), POOL, CFG
        );
    }

    function _fund(OnyxParametric p, address who, uint256 a) internal {
        tok.mint(who, a);
        vm.prank(who);
        tok.approve(address(p), type(uint256).max);
    }

    function _underwrite(address who, uint256 a) internal {
        _fund(ins, who, a);
        vm.prank(who);
        ins.underwrite(a);
    }

    function _buy(uint256 id, address who, uint32 n) internal {
        _fund(ins, who, uint256(ins.product(id).premium) * n);
        vm.prank(who);
        ins.buy(id, n);
    }

    /// "Denver precipitation on the day is at least 10 mm" (tenths of mm: 100).
    function _offer(bytes32 fact) internal returns (uint256) {
        return ins.offer(
            fact, OnyxParametric.Cmp.AtLeast, 100, 100e18, 10e18, buyBy, observable, deadline, 50
        );
    }

    function _settleWith(bytes32 fact, uint256 id, int128 value) internal {
        vm.warp(observable);
        vm.roll(block.number + 20);
        _record(value);
        facts.prove(fact, ROUND, value);
        ins.settle(id);
    }

    function test_APolicyPaysWhenTheValueCrossesTheLine() public {
        bytes32 fact = _list();
        _underwrite(uma, 1_000e18);
        uint256 id = _offer(fact);
        _buy(id, bob, 2);
        assertEq(ins.reserved(), 200e18, "payout reserved");
        _settleWith(fact, id, 123);
        assertEq(uint8(ins.product(id).state), uint8(OnyxParametric.State.Paid));
        assertEq(ins.product(id).value, 123);
        vm.prank(bob);
        ins.claim(id);
        assertEq(tok.balanceOf(bob), 200e18, "two payouts");
        assertEq(ins.reserved(), 0);
        assertEq(ins.pool(), 1_000e18 + 20e18 - 200e18, "premiums in, payouts out");
        uint256 s = ins.shares(uma);
        vm.prank(uma);
        ins.withdraw(s);
        assertEq(tok.balanceOf(uma), 820e18);
        assertEq(tok.balanceOf(address(ins)), 0, "nothing left behind");
    }

    function test_APolicyThatDoesNotTriggerExpires() public {
        bytes32 fact = _list();
        _underwrite(uma, 1_000e18);
        uint256 id = _offer(fact);
        _buy(id, bob, 2);
        _settleWith(fact, id, 99);
        assertEq(uint8(ins.product(id).state), uint8(OnyxParametric.State.Expired));
        assertEq(ins.owed(id, bob), 0);
        vm.prank(bob);
        vm.expectRevert(OnyxParametric.NothingOwed.selector);
        ins.claim(id);
        assertEq(ins.pool(), 1_020e18, "the premiums are earned");
        assertEq(ins.reserved(), 0, "the reserve is released");
    }

    function test_AtMostTriggersBelowTheLine() public {
        bytes32 fact = _list();
        _underwrite(uma, 1_000e18);
        uint256 id = ins.offer(
            fact, OnyxParametric.Cmp.AtMost, 0, 100e18, 10e18, buyBy, observable, deadline, 50
        );
        _buy(id, bob, 1);
        _settleWith(fact, id, 0);
        assertEq(uint8(ins.product(id).state), uint8(OnyxParametric.State.Paid), "a dry day");
    }

    function test_NoValueByTheDeadlineRefundsThePremium() public {
        bytes32 fact = _list();
        _underwrite(uma, 1_000e18);
        uint256 id = _offer(fact);
        _buy(id, bob, 3);
        vm.warp(observable);
        vm.expectRevert(OnyxParametric.WrongPhase.selector);
        ins.settle(id);
        vm.warp(deadline);
        ins.settle(id);
        assertEq(uint8(ins.product(id).state), uint8(OnyxParametric.State.Void));
        vm.prank(bob);
        ins.claim(id);
        assertEq(tok.balanceOf(bob), 30e18, "premiums back");
        assertEq(ins.pool(), 1_000e18);
        assertEq(ins.reserved(), 0);
    }

    /// R-PI-1 (THREATS NL6-10): buying closes no later than the listing's `closeBy`.
    function test_BuyingClosesBeforeTheSourcePublishes() public {
        bytes32 fact = _list();
        vm.expectRevert(OnyxParametric.BadProduct.selector);
        ins.offer(
            fact, OnyxParametric.Cmp.AtLeast, 1, 100e18, 10e18, buyBy + 1, observable, deadline, 5
        );
        vm.expectRevert(OnyxParametric.BadProduct.selector);
        ins.offer(
            keccak256("unlisted"),
            OnyxParametric.Cmp.AtLeast,
            1,
            100e18,
            10e18,
            buyBy,
            observable,
            deadline,
            5
        );
        _underwrite(uma, 1_000e18);
        uint256 id = _offer(fact);
        vm.warp(buyBy);
        _fund(ins, bob, 10e18);
        vm.prank(bob);
        vm.expectRevert(OnyxParametric.WrongPhase.selector);
        ins.buy(id, 1);
    }

    function test_OfferRules() public {
        bytes32 fact = _list();
        vm.prank(bob);
        vm.expectRevert(OnyxParametric.NotOwner.selector);
        _offer(fact);
        // The payout must exceed the premium; buying must close a lead before observation.
        vm.expectRevert(OnyxParametric.BadProduct.selector);
        ins.offer(fact, OnyxParametric.Cmp.AtLeast, 1, 10e18, 10e18, buyBy, observable, deadline, 5);
        vm.expectRevert(OnyxParametric.BadProduct.selector);
        ins.offer(
            fact,
            OnyxParametric.Cmp.AtLeast,
            1,
            100e18,
            10e18,
            buyBy,
            buyBy + 59 minutes,
            deadline,
            5
        );
        vm.expectRevert(OnyxParametric.BadProduct.selector);
        ins.offer(
            fact, OnyxParametric.Cmp.AtLeast, 1, 100e18, 10e18, buyBy, observable, observable, 5
        );
        vm.expectRevert(OnyxParametric.BadProduct.selector);
        ins.offer(
            fact, OnyxParametric.Cmp.AtLeast, 1, 100e18, 10e18, buyBy, observable, deadline, 0
        );
    }

    /// R-PI-4 (THREATS NL6-12): the owner, the registry's coordinator, the listing owner and
    /// re-observation observers may neither buy nor underwrite.
    function test_InsidersMayNotBuyOrUnderwrite() public {
        assertFalse(ins.mayTakePart(address(this)), "owner and coordinator");
        ro.addObserver(cat);
        assertFalse(ins.mayTakePart(cat), "an observer");
        assertTrue(ins.mayTakePart(bob));
        OnyxObservedFacts f2 = new OnyxObservedFacts(reg, vic);
        OnyxParametric p2 = new OnyxParametric(
            IStakeToken(address(tok)),
            IOnyxFacts(address(f2)),
            IOnyxFactGuard(address(f2)),
            IOnyxReobserve(address(ro))
        );
        assertFalse(p2.mayTakePart(vic), "the listing owner");
        _fund(ins, cat, 100e18);
        vm.prank(cat);
        vm.expectRevert(OnyxParametric.NotAllowed.selector);
        ins.underwrite(100e18);
        bytes32 fact = _list();
        _underwrite(uma, 1_000e18);
        uint256 id = _offer(fact);
        tok.mint(address(this), 10e18);
        tok.approve(address(ins), 10e18);
        vm.expectRevert(OnyxParametric.NotAllowed.selector);
        ins.buy(id, 1);
    }

    function test_AGuardOtherThanTheFactsIsRefused() public {
        address other = address(new OnyxObservedFacts(reg, vic));
        vm.expectRevert(OnyxParametric.BadProduct.selector);
        new OnyxParametric(
            IStakeToken(address(tok)),
            IOnyxFacts(address(facts)),
            IOnyxFactGuard(other),
            IOnyxReobserve(address(ro))
        );
    }

    /// R-PI-5: a policy is sold only when the free pool covers its payout.
    function test_APolicyIsSoldOnlyWhenThePoolCoversIt() public {
        bytes32 fact = _list();
        _underwrite(uma, 150e18);
        uint256 id = _offer(fact);
        _buy(id, bob, 1);
        _fund(ins, cat, 10e18);
        vm.prank(cat);
        vm.expectRevert(OnyxParametric.NotCovered.selector);
        ins.buy(id, 1);
        // Per-buyer cap.
        _underwrite(vic, 10_000e18);
        _fund(ins, cat, 110e18);
        vm.prank(cat);
        vm.expectRevert(OnyxParametric.BadAmount.selector);
        ins.buy(id, 11);
    }

    /// THREATS NL18-4: an underwriter leaving takes only its part of the free money, so a coming
    /// payout stays covered and lands on nobody who stayed more than their share.
    function test_AnUnderwriterLeavingTakesOnlyFreeMoney() public {
        bytes32 fact = _list();
        _underwrite(uma, 1_000e18);
        _underwrite(vic, 1_000e18);
        uint256 id = _offer(fact);
        _buy(id, bob, 5);
        assertEq(ins.reserved(), 500e18);
        uint256 s = ins.shares(uma);
        vm.prank(uma);
        ins.withdraw(s);
        assertEq(tok.balanceOf(uma), 750e18, "half of the 1,500 free");
        _settleWith(fact, id, 150);
        vm.prank(bob);
        ins.claim(id);
        assertEq(tok.balanceOf(bob), 500e18, "the payout is still covered");
        uint256 sv = ins.shares(vic);
        vm.prank(vic);
        ins.withdraw(sv);
        // 2,000 + 50 premiums - 750 - 500.
        assertEq(tok.balanceOf(vic), 800e18);
    }

    /// R-PI-3: a value decided by a stake vote or the panel (level 4) never settles a policy;
    /// with no acceptable value the product voids at `resolveBy`.
    function test_AVoteDecidedValueNeverSettles() public {
        SetFacts sf = new SetFacts(address(0xBAD));
        OnyxParametric p = new OnyxParametric(
            IStakeToken(address(tok)),
            IOnyxFacts(address(sf)),
            IOnyxFactGuard(address(sf)),
            IOnyxReobserve(address(ro))
        );
        bytes32 fact = keccak256("fact");
        sf.listUntil(fact, buyBy);
        _fund(p, uma, 1_000e18);
        vm.prank(uma);
        p.underwrite(1_000e18);
        uint256 id = p.offer(
            fact, OnyxParametric.Cmp.AtLeast, 1, 100e18, 10e18, buyBy, observable, deadline, 5
        );
        _fund(p, bob, 10e18);
        vm.prank(bob);
        p.buy(id, 1);
        vm.warp(observable);
        sf.set(fact, 5, 4);
        vm.expectRevert(OnyxParametric.WrongPhase.selector);
        p.settle(id);
        vm.warp(deadline);
        p.settle(id);
        assertEq(uint8(p.product(id).state), uint8(OnyxParametric.State.Void));
        assertFalse(p.mayTakePart(address(0xBAD)), "the source's insider");
    }

    /// R-PI-2 (THREATS NL6-11): a value accepted before `observableAt` never settles.
    function test_AValueAcceptedBeforeObservableAtNeverSettles() public {
        SetFacts sf = new SetFacts(address(0));
        OnyxParametric p = new OnyxParametric(
            IStakeToken(address(tok)),
            IOnyxFacts(address(sf)),
            IOnyxFactGuard(address(sf)),
            IOnyxReobserve(address(ro))
        );
        bytes32 fact = keccak256("fact");
        sf.listUntil(fact, buyBy);
        _fund(p, uma, 1_000e18);
        vm.prank(uma);
        p.underwrite(1_000e18);
        uint256 id = p.offer(
            fact, OnyxParametric.Cmp.AtLeast, 1, 100e18, 10e18, buyBy, observable, deadline, 5
        );
        vm.warp(observable - 1);
        sf.set(fact, 5, 2);
        vm.warp(observable);
        vm.expectRevert(OnyxParametric.WrongPhase.selector);
        p.settle(id);
    }

    /// Money sent to the contract directly is never counted, so it cannot move the share price.
    function test_ADonationDoesNotMoveTheSharePrice() public {
        _underwrite(uma, 100e18);
        tok.mint(address(ins), 1_000_000e18);
        _underwrite(vic, 100e18);
        assertEq(ins.shares(vic), ins.shares(uma));
    }

    function test_AnEmptiedPoolPricesNewMoneyForItsDepositor() public {
        bytes32 fact = _list();
        _underwrite(uma, 100e18);
        uint256 id = _offer(fact);
        _buy(id, bob, 1);
        _settleWith(fact, id, 500);
        vm.prank(bob);
        ins.claim(id);
        // 100 + 10 premium - 100 payout = 10 left; then a new depositor.
        _underwrite(vic, 1_000e18);
        uint256 sv = ins.shares(vic);
        vm.prank(vic);
        ins.withdraw(sv);
        assertGe(tok.balanceOf(vic), 999e18, "the new money stays with its depositor");
    }
}
