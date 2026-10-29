// SPDX-License-Identifier: Apache-2.0
pragma solidity ^0.8.24;

import {Test} from "forge-std/Test.sol";
import {OnyxRegistry, IRegistryLedger} from "../src/OnyxRegistry.sol";
import {OnyxObservedFacts} from "../src/OnyxObservedFacts.sol";
import {OnyxTestToken} from "../src/OnyxTestToken.sol";
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

/// The test is the settlement's coordinator, so it records level-2 facts itself.
contract CoordinatorLedger {
    address internal immutable c;

    constructor(address c_) {
        c = c_;
    }

    function coordinator() external view returns (address) {
        return c;
    }

    function coordinatorEpoch() external pure returns (uint64) {
        return 0;
    }
}

/// NS11 (ADR-0042 amendment 2): a futures market settles from a level-2 value the registry
/// recorded, proved by its round id and number against the recorded result hash.
contract OnyxObservedFactsTest is Test {
    OnyxRegistry internal reg;
    OnyxObservedFacts internal facts;
    OnyxTestToken internal tok;
    OnyxFutures internal fut;

    address internal gov = address(0x60F);
    address internal treasury = address(0x7EA5);
    address internal dave = makeAddr("dave");
    address internal erin = makeAddr("erin");

    bytes32 internal constant METRIC = keccak256("us.treasury.total_public_debt_outstanding");
    bytes32 internal constant TEMPLATE = keccak256("treasury template");
    bytes32 internal constant SUBJECT = keccak256("us.treasury.debt:2026-10-27");
    uint64 internal constant WINDOW = 1_793_250_000;
    bytes32 internal constant POOL = keccak256("pool");
    bytes32 internal constant CFG = keccak256("cfg");
    bytes32 internal constant SPEC = keccak256("spec");
    bytes32 internal constant ROUND = keccak256("round");
    int128 internal constant VALUE = 3_812_345_678_901_234;

    uint64 internal close;
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
        fut = new OnyxFutures(
            IStakeToken(address(tok)),
            IOnyxFacts(address(facts)),
            IOnyxReobserve(address(new OnyxReobserve(0))),
            IOnyxFallbackOracle(address(new OnyxTestnetOwnerOracle())),
            IOnyxFactGuard(address(facts)),
            treasury,
            OnyxFutures.Params({
                minBet: 1e18,
                maxBetPerAddress: 100e18,
                maxPoolPerMarket: 10_000e18,
                feePpm: 20_000,
                minCloseLead: 60,
                maxBuckets: 8,
                maxLevel: 2
            })
        );
        close = uint64(block.timestamp) + 1 days;
        observable = close + 12 hours;
        deadline = observable + 7 days;
    }

    function _list(uint64 notBefore) internal returns (bytes32) {
        return facts.list(METRIC, TEMPLATE, SUBJECT, 0, true, notBefore, close);
    }

    /// Announce and fix a draw committed to `SPEC`, then record `value` for `ROUND`.
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

    function _bet(uint256 id, address who, uint8 b, uint128 a) internal {
        tok.mint(who, a);
        vm.startPrank(who);
        tok.approve(address(fut), a);
        fut.bet(id, b, a);
        vm.stopPrank();
    }

    function _edges() internal pure returns (int256[] memory e) {
        e = new int256[](1);
        e[0] = 3_800_000_000_000_000;
    }

    function test_AMarketSettlesFromTheRecordedLevel2Value() public {
        bytes32 fact = _list(uint64(block.number) + 10);
        assertEq(fact, reg.factKey(METRIC, SUBJECT, WINDOW), "the registry's own key");
        uint256 id = fut.createMarket(fact, _edges(), close, observable, deadline);
        _bet(id, dave, 0, 40e18);
        _bet(id, erin, 1, 60e18);
        (bool fin,,,) = facts.factValue(fact);
        assertFalse(fin, "no value before the round");
        vm.warp(observable);
        vm.roll(block.number + 20);
        _record(VALUE);
        facts.prove(fact, ROUND, VALUE);
        (bool f2, int256 v, uint8 level, uint64 at) = facts.factValue(fact);
        assertTrue(f2);
        assertEq(v, VALUE);
        assertEq(level, 2, "re-observed");
        assertEq(at, uint64(block.timestamp));
        assertEq(facts.roundOf(fact), ROUND);
        fut.resolve(id);
        assertEq(uint8(fut.market(id).state), uint8(OnyxFutures.State.Resolved));
        assertEq(fut.market(id).winner, 1, "over the edge");
        vm.prank(erin);
        fut.withdraw(id);
        // erin's 60 back plus dave's 40 less the 2% fee.
        assertEq(tok.balanceOf(erin), 60e18 + 40e18 - 0.8e18);
        assertEq(fut.owed(id, dave), 0);
    }

    function test_AWrongValueOrRoundNeverProves() public {
        bytes32 fact = _list(uint64(block.number) + 1);
        vm.roll(block.number + 5);
        _record(VALUE);
        vm.expectRevert(OnyxObservedFacts.WrongValue.selector);
        facts.prove(fact, ROUND, VALUE + 1);
        vm.expectRevert(OnyxObservedFacts.WrongValue.selector);
        facts.prove(fact, keccak256("another round"), VALUE);
        facts.prove(fact, ROUND, VALUE);
        vm.expectRevert(OnyxObservedFacts.AlreadyProved.selector);
        facts.prove(fact, ROUND, VALUE);
    }

    function test_ARecordingBeforeTheListedBlockNeverProves() public {
        bytes32 fact = _list(uint64(block.number) + 100);
        _record(VALUE);
        vm.expectRevert(OnyxObservedFacts.TooEarly.selector);
        facts.prove(fact, ROUND, VALUE);
    }

    function test_AValueAlreadyOnChainCannotBeListed() public {
        _record(VALUE);
        vm.expectRevert(OnyxObservedFacts.AlreadyRecorded.selector);
        _list(uint64(block.number) + 1);
    }

    function test_ListingRules() public {
        vm.prank(dave);
        vm.expectRevert(OnyxObservedFacts.NotOwner.selector);
        _list(uint64(block.number) + 1);
        vm.expectRevert(OnyxObservedFacts.BadFact.selector);
        _list(uint64(block.number));
        vm.expectRevert(OnyxObservedFacts.BadFact.selector);
        facts.list(METRIC, TEMPLATE, bytes32(0), 0, true, uint64(block.number) + 1, close);
        vm.expectRevert(OnyxObservedFacts.BadFact.selector);
        facts.list(METRIC, TEMPLATE, SUBJECT, 0, true, uint64(block.number) + 1, 0);
        _list(uint64(block.number) + 1);
        vm.expectRevert(OnyxObservedFacts.AlreadyListed.selector);
        _list(uint64(block.number) + 1);
        vm.expectRevert(OnyxObservedFacts.NotListed.selector);
        facts.prove(keccak256("unlisted"), ROUND, VALUE);
    }

    function test_NoResultYetNeverProves() public {
        bytes32 fact = _list(uint64(block.number) + 1);
        vm.expectRevert(OnyxObservedFacts.NoResult.selector);
        facts.prove(fact, ROUND, VALUE);
    }

    /// A fact listed under another template than the registered one never proves: the
    /// registry's key differs, so no recorded result can stand in for it.
    function test_AnotherTemplateNeverProves() public {
        bytes32 fact = facts.list(
            METRIC, keccak256("other template"), SUBJECT, 0, true, uint64(block.number) + 1, close
        );
        vm.roll(block.number + 5);
        _record(VALUE);
        vm.expectRevert(OnyxObservedFacts.BadFact.selector);
        facts.prove(fact, ROUND, VALUE);
    }

    /// THREATS NL6-12: the futures owner and treasury, the listing owner and the registry's
    /// coordinator decide or delay the value, so none may bet; an outsider may.
    function test_TheCoordinatorAndTheOwnerMayNotBet() public {
        assertFalse(fut.mayBet(address(this)), "coordinator, listing and futures owner");
        assertFalse(fut.mayBet(treasury), "treasury");
        assertTrue(fut.mayBet(dave));
        OnyxObservedFacts other = new OnyxObservedFacts(reg, erin);
        assertTrue(other.isInsider(erin), "the listing owner");
        assertTrue(other.isInsider(address(this)), "the registry's coordinator");
        assertFalse(other.isInsider(dave));
        bytes32 fact = _list(uint64(block.number) + 10);
        uint256 id = fut.createMarket(fact, _edges(), close, observable, deadline);
        tok.mint(address(this), 10e18);
        tok.approve(address(fut), 10e18);
        vm.expectRevert(OnyxFutures.NotAllowed.selector);
        fut.bet(id, 0, 10e18);
    }

    /// THREATS NL6-10: trading may not close after the listing's `closeBy` (the source's
    /// earliest publication less a margin), and a fact the guard never listed opens no market.
    function test_ACloseAfterTheSourcePublishesIsRefused() public {
        bytes32 fact =
            facts.list(METRIC, TEMPLATE, SUBJECT, 0, true, uint64(block.number) + 10, close - 1);
        assertEq(facts.closeBy(fact), close - 1);
        vm.expectRevert(OnyxFutures.BadMarket.selector);
        fut.createMarket(fact, _edges(), close, observable, deadline);
        fut.createMarket(fact, _edges(), close - 1, observable, deadline);
        vm.expectRevert(OnyxFutures.BadMarket.selector);
        fut.createMarket(keccak256("unlisted"), _edges(), close, observable, deadline);
    }

    function test_AGuardOtherThanTheFactsIsRefused() public {
        address ro = address(new OnyxReobserve(0));
        address orc = address(new OnyxTestnetOwnerOracle());
        address other = address(new OnyxObservedFacts(reg, erin));
        OnyxFutures.Params memory p = fut.defaultParams();
        vm.expectRevert(OnyxFutures.BadParams.selector);
        new OnyxFutures(
            IStakeToken(address(tok)),
            IOnyxFacts(address(facts)),
            IOnyxReobserve(ro),
            IOnyxFallbackOracle(orc),
            IOnyxFactGuard(other),
            treasury,
            p
        );
    }

    /// THREATS NL6-11: the fact is listed with `notBeforeBlock` at `observableAt`, so a
    /// recording made earlier (a mistaken or cheating coordinator) never proves; the market
    /// cannot resolve on it and voids at `resolveBy`, refunding every bet.
    function test_ARecordingBeforeObservableAtCannotBrickAMarket() public {
        // About 2 s blocks: the block at `observableAt`.
        uint64 atObservable = uint64(block.number) + (observable - uint64(block.timestamp)) / 2;
        bytes32 fact = _list(atObservable);
        uint256 id = fut.createMarket(fact, _edges(), close, observable, deadline);
        _bet(id, dave, 0, 40e18);
        _bet(id, erin, 1, 60e18);
        vm.warp(close + 1);
        vm.roll(block.number + 10);
        _record(VALUE);
        vm.expectRevert(OnyxObservedFacts.TooEarly.selector);
        facts.prove(fact, ROUND, VALUE);
        vm.warp(observable + 1);
        vm.expectRevert(OnyxFutures.WrongPhase.selector);
        fut.resolve(id);
        vm.warp(deadline);
        fut.resolve(id);
        assertEq(uint8(fut.market(id).state), uint8(OnyxFutures.State.Void));
        vm.prank(dave);
        fut.withdraw(id);
        assertEq(tok.balanceOf(dave), 40e18, "refunded");
    }

    function testFuzz_OnlyTheRecordedNumberProves(int128 recorded, int128 claimed) public {
        bytes32 fact = _list(uint64(block.number) + 1);
        vm.roll(block.number + 5);
        _record(recorded);
        if (claimed != recorded) {
            vm.expectRevert(OnyxObservedFacts.WrongValue.selector);
            facts.prove(fact, ROUND, claimed);
        }
        facts.prove(fact, ROUND, recorded);
        (, int256 v,,) = facts.factValue(fact);
        assertEq(v, recorded);
    }
}

/// Rust <-> Solidity (fixtures/markets/observed-facts.json): `result_hash_of` against
/// `OnyxObservedFacts.resultHash`, the recorded Treasury round's settlement first.
contract OnyxObservedFactsVectorsTest is Test {
    function test_RustVectorsMatch() public {
        string memory json = vm.readFile("../fixtures/markets/observed-facts.json");
        OnyxObservedFacts f = new OnyxObservedFacts(OnyxRegistry(address(1)), address(1));
        bytes32[] memory specs = vm.parseJsonBytes32Array(json, ".specs");
        bytes32[] memory rounds = vm.parseJsonBytes32Array(json, ".rounds");
        string[] memory values = vm.parseJsonStringArray(json, ".values");
        bytes32[] memory results = vm.parseJsonBytes32Array(json, ".results");
        assertEq(specs.length, rounds.length);
        assertEq(specs.length, values.length);
        assertEq(specs.length, results.length);
        assertGt(specs.length, 6);
        for (uint256 i = 0; i < specs.length; i++) {
            int128 v = int128(vm.parseInt(values[i]));
            assertEq(f.resultHash(specs[i], rounds[i], v), results[i], values[i]);
        }
    }
}
